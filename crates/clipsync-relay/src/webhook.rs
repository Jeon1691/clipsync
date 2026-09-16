use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

pub async fn post_event(url: &str, secret: &str, event: serde_json::Value) -> anyhow::Result<()> {
    let parsed = Url::parse(url)?;
    if is_blocked(&parsed) {
        anyhow::bail!("webhook url blocked by SSRF policy");
    }
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
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let _ = client
        .post(url)
        .header("X-ClipSync-Timestamp", &ts)
        .header("X-ClipSync-Event-Id", &event_id)
        .header("X-ClipSync-Signature", format!("sha256={sig}"))
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await?;
    Ok(())
}

fn is_blocked(url: &Url) -> bool {
    if url.scheme() != "https" && url.scheme() != "http" {
        return true;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d == "metadata.google.internal"
        }
        Some(url::Host::Ipv4(ip)) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.octets()[0] == 169 && ip.octets()[1] == 254
        }
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        None => true,
    }
}

#[cfg(test)]
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
}
