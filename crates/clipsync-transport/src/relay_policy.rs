// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Relay URL transport-security policy shared by configuration and connect.
//!
//! Pairing codes (the SPAKE2 password), pairing tokens and long-lived device
//! tokens travel to the relay in HTTP bodies and WebSocket hello frames. Over
//! plaintext an on-path attacker can read them and impersonate the relay
//! during pairing, so clients require `https://`/`wss://` except for loopback
//! relays on this machine or an explicit, logged opt-in.

use url::{Host, Url};

/// Setting this to [`ALLOW_INSECURE_RELAY_ENABLED`] permits plaintext
/// `http://`/`ws://` relays on non-loopback hosts (for example, a trusted
/// private network); every such use is logged as a warning.
pub const ALLOW_INSECURE_RELAY_ENV: &str = "CLIPSYNC_ALLOW_INSECURE_RELAY";

/// The only value that enables the opt-in, so an accidental `0`/`false`
/// never weakens transport security.
const ALLOW_INSECURE_RELAY_ENABLED: &str = "1";

/// The only hostname treated as loopback without resolving it.
const LOOPBACK_HOSTNAME: &str = "localhost";

/// How a relay URL that passed the policy is protected in transit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelaySecurity {
    /// `https://` or `wss://` with certificate verification.
    Tls,
    /// Plaintext, but the relay is on this machine.
    Loopback,
    /// Plaintext to another host because the user set the opt-in.
    InsecureOptIn,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RelayUrlError {
    #[error("relay URL has no scheme; write it as https://<host> (for example https://relay.example.com)")]
    MissingScheme,
    #[error("relay URL scheme `{scheme}` is not supported; use https://")]
    UnsupportedScheme { scheme: String },
    #[error("relay URL has no host; write it as https://<host>")]
    MissingHost,
    #[error("relay URL is invalid: {0}")]
    Invalid(url::ParseError),
    #[error(
        "refusing plaintext {scheme}:// relay at {host}: pairing codes and device tokens would be sent unencrypted. \
         Use https:// (or wss://). Plaintext is only allowed for loopback relays (127.0.0.0/8, ::1, localhost); \
         set {ALLOW_INSECURE_RELAY_ENV}={ALLOW_INSECURE_RELAY_ENABLED} to override this on a network you fully trust"
    )]
    Plaintext { scheme: String, host: String },
    #[error(
        "relay URL host `{written}` is not in canonical form; write it as `{canonical}` so the checked host is the one connected to"
    )]
    NonCanonicalHost { written: String, canonical: String },
}

/// Whether an [`ALLOW_INSECURE_RELAY_ENV`] value enables the opt-in.
pub fn insecure_relay_opt_in(value: Option<&str>) -> bool {
    value == Some(ALLOW_INSECURE_RELAY_ENABLED)
}

/// Classify `url` against the relay transport policy without side effects.
pub fn relay_url_security(url: &str, allow_insecure: bool) -> Result<RelaySecurity, RelayUrlError> {
    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(url::ParseError::RelativeUrlWithoutBase) => return Err(RelayUrlError::MissingScheme),
        Err(err) => return Err(RelayUrlError::Invalid(err)),
    };
    let plaintext = match parsed.scheme() {
        "https" | "wss" => false,
        "http" | "ws" => true,
        // `host:port` parses as scheme `host`; report it as a missing scheme
        // instead of guessing one.
        _ if !url.contains("://") => return Err(RelayUrlError::MissingScheme),
        scheme => {
            return Err(RelayUrlError::UnsupportedScheme {
                scheme: scheme.into(),
            })
        }
    };
    let host = parsed.host().ok_or(RelayUrlError::MissingHost)?;
    if !plaintext {
        return Ok(RelaySecurity::Tls);
    }
    if is_loopback(&host) {
        return Ok(RelaySecurity::Loopback);
    }
    if allow_insecure {
        return Ok(RelaySecurity::InsecureOptIn);
    }
    Err(RelayUrlError::Plaintext {
        scheme: parsed.scheme().into(),
        host: host.to_string(),
    })
}

/// Enforce the relay transport policy, honoring [`ALLOW_INSECURE_RELAY_ENV`]
/// and warning whenever the opt-in lets a plaintext URL through.
pub fn check_relay_url(url: &str) -> Result<RelaySecurity, RelayUrlError> {
    let allow_insecure =
        insecure_relay_opt_in(std::env::var(ALLOW_INSECURE_RELAY_ENV).ok().as_deref());
    let security = relay_url_security(url, allow_insecure)?;
    if security == RelaySecurity::InsecureOptIn {
        let host = Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_default();
        tracing::warn!(
            %host,
            "{ALLOW_INSECURE_RELAY_ENV} is set: using a plaintext relay; pairing codes and device tokens are sent unencrypted"
        );
    }
    Ok(security)
}

/// Require the host the WebSocket client will dial (`connect_host`, taken from
/// the raw URL) to equal the host the policy classified. `url` normalizes
/// forms such as `127.0.0.1.` or `0x7f.1` to `127.0.0.1`, while the socket
/// layer may resolve the raw text through DNS, which an on-path attacker can
/// answer.
pub(crate) fn require_canonical_host(
    url: &str,
    connect_host: Option<&str>,
) -> Result<(), RelayUrlError> {
    let canonical = Url::parse(url)
        .map_err(RelayUrlError::Invalid)?
        .host_str()
        .ok_or(RelayUrlError::MissingHost)?
        .to_owned();
    let written = connect_host.ok_or(RelayUrlError::MissingHost)?;
    if written.eq_ignore_ascii_case(&canonical) {
        Ok(())
    } else {
        Err(RelayUrlError::NonCanonicalHost {
            written: written.into(),
            canonical,
        })
    }
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
        // `url` lowercases domains; subdomains of localhost are not trusted
        // because resolving them is up to the network's resolver.
        Host::Domain(domain) => *domain == LOOPBACK_HOSTNAME,
    }
}
