// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::Context;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

type HmacSha256 = Hmac<Sha256>;
const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn post_event(url: &str, secret: &str, event: serde_json::Value) -> anyhow::Result<()> {
    let parsed = Url::parse(url)?;
    if is_blocked(&parsed) {
        anyhow::bail!("webhook url blocked by SSRF policy");
    }
    // Resolve once per delivery, reject the entire answer if any address is
    // non-public, and connect only to this validated snapshot. Keeping the URL
    // hostname preserves normal certificate verification, SNI and Host headers.
    let addresses = resolve_addresses(&parsed).await?;
    let client = webhook_client(
        parsed.host_str().context("webhook host missing")?,
        &addresses,
    )?;
    let body = serde_json::to_vec(&event)?;
    let ts = chrono::Utc::now().timestamp().to_string();
    let event_id = ulid::Ulid::new().to_string();
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|e| anyhow::anyhow!("{e}"))?;
    mac.update(ts.as_bytes());
    mac.update(b".");
    mac.update(event_id.as_bytes());
    mac.update(b".");
    mac.update(&body);
    let sig = hex::encode(mac.finalize().into_bytes());
    let _ = client
        .post(parsed)
        .header("X-ClipSync-Timestamp", &ts)
        .header("X-ClipSync-Event-Id", &event_id)
        .header("X-ClipSync-Signature", format!("sha256={sig}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    Ok(())
}

async fn resolve_addresses(url: &Url) -> anyhow::Result<Vec<SocketAddr>> {
    let port = url
        .port_or_known_default()
        .context("webhook port missing")?;
    let addresses = match url.host().context("webhook host missing")? {
        url::Host::Domain(host) => {
            tokio::time::timeout(WEBHOOK_TIMEOUT, tokio::net::lookup_host((host, port)))
                .await
                .context("webhook DNS resolution timed out")??
                .collect()
        }
        url::Host::Ipv4(ip) => vec![SocketAddr::new(ip.into(), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(ip.into(), port)],
    };
    validate_addresses(&addresses)?;
    Ok(addresses)
}

fn validate_addresses(addresses: &[SocketAddr]) -> anyhow::Result<()> {
    if addresses.is_empty() || addresses.iter().any(|address| !is_public(address.ip())) {
        anyhow::bail!("webhook destination blocked by SSRF policy");
    }
    Ok(())
}

fn webhook_client(host: &str, addresses: &[SocketAddr]) -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        // A proxy could resolve the name again or route to a different target.
        .no_proxy()
        .resolve_to_addrs(host, addresses)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(WEBHOOK_TIMEOUT)
        .build()
}

fn is_blocked(url: &Url) -> bool {
    if url.scheme() != "https" && url.scheme() != "http" {
        return true;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d == "metadata.google.internal"
        }
        Some(url::Host::Ipv4(ip)) => !is_public(ip.into()),
        Some(url::Host::Ipv6(ip)) => !is_public(ip.into()),
        None => true,
    }
}

fn is_public(ip: IpAddr) -> bool {
    // Conservatively exclude special-purpose, translation and transition ranges
    // as well as private/local addresses. Registry references:
    // https://www.iana.org/assignments/iana-ipv4-special-registry/
    // https://www.iana.org/assignments/iana-ipv6-special-registry/
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_documentation()
                || a >= 224
                || a == 100 && (64..=127).contains(&b)
                || a == 192 && b == 0 && c == 0
                || a == 192 && b == 88 && c == 99
                || a == 198 && (b == 18 || b == 19))
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            // Ordinary global unicast only; this also rejects mapped IPv4,
            // NAT64, multicast, ULA, link-local and currently reserved space.
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && segments[1] < 0x0200)
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
                && segments[0] != 0x2002
                && segments[0] != 0x3ffe
                && !(segments[0] == 0x3fff && segments[1] & 0xf000 == 0)
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn blocks_private() {
        assert!(is_blocked(&Url::parse("http://127.0.0.1/hook").unwrap()));
        assert!(is_blocked(&Url::parse("http://10.0.0.1/hook").unwrap()));
        assert!(is_blocked(&Url::parse("http://localhost/hook").unwrap()));
        assert!(!is_blocked(
            &Url::parse("https://example.com/hook").unwrap()
        ));
    }

    #[test]
    fn blocks_non_public_literals() {
        for host in [
            "0.1.2.3",
            "100.64.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.2",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "[fc00::1]",
            "[fd00::1]",
            "[fe80::1]",
            "[ff02::1]",
            "[::ffff:127.0.0.1]",
            "[64:ff9b::a00:1]",
            "[2001::1]",
            "[2001:db8::1]",
            "[2002:a00:1::1]",
            "[3fff::1]",
        ] {
            let url = Url::parse(&format!("https://{host}/hook")).unwrap();
            assert!(is_blocked(&url), "non-public address allowed: {host}");
        }
    }

    #[test]
    fn rejects_private_or_mixed_dns_answers() {
        let public = "8.8.8.8:443".parse().unwrap();
        for private in [
            "127.0.0.1:443",
            "10.0.0.1:443",
            "[fd00::1]:443",
            "[fe80::1]:443",
        ] {
            let private = private.parse().unwrap();
            assert!(validate_addresses(&[private]).is_err());
            assert!(validate_addresses(&[public, private]).is_err());
        }
        assert!(validate_addresses(&[]).is_err());
        assert!(
            validate_addresses(&[public, "[2606:4700:4700::1111]:443".parse().unwrap()]).is_ok()
        );
    }

    #[tokio::test]
    async fn rejects_a_hostname_resolving_to_loopback() {
        let url = Url::parse("https://localhost/hook").unwrap();
        assert!(resolve_addresses(&url).await.is_err());
    }

    #[test]
    fn allows_ordinary_public_webhook_urls() {
        for url in [
            "https://example.com/hook",
            "https://8.8.8.8/hook",
            "https://[2606:4700:4700::1111]/hook",
            "http://example.com/hook",
        ] {
            assert!(
                !is_blocked(&Url::parse(url).unwrap()),
                "blocked public URL: {url}"
            );
        }
        for url in [
            "http://localhost./hook",
            "http://name.localhost./hook",
            "http://metadata.google.internal./hook",
            "ftp://example.com/hook",
        ] {
            assert!(
                is_blocked(&Url::parse(url).unwrap()),
                "allowed unsafe URL: {url}"
            );
        }
    }

    #[tokio::test]
    async fn client_pins_addresses_preserves_host_and_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let redirected = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirected_address = redirected.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let length = socket.read(&mut chunk).await.unwrap();
                assert_ne!(length, 0, "request closed before headers");
                request.extend_from_slice(&chunk[..length]);
                assert!(request.len() < 32768, "unexpectedly large test request");
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("POST /hook HTTP/1.1\r\n"));
            assert!(request
                .to_ascii_lowercase()
                .contains(&format!("\r\nhost: webhook.invalid:{}\r\n", address.port())));
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{redirected_address}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        // Only this transport test supplies a local socket directly. Production
        // validates DNS answers before constructing this client.
        let client = webhook_client("webhook.invalid", &[address]).unwrap();
        let response = client
            .post(format!("http://webhook.invalid:{}/hook", address.port()))
            .body("synthetic")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), redirected.accept())
                .await
                .is_err()
        );
    }
}
