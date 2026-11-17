// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
//! Start the real relay binary on a port the operating system assigns to the
//! child itself, and learn that port from the child.
//!
//! Reserving a port in the test (bind `:0`, read it, drop the listener, pass
//! it to the child) leaves a window in which a parallel test or process can
//! take the port. The child then fails to bind, or the test's readiness probe
//! reaches the other listener and the test talks to the wrong server. Binding
//! `:0` in the child closes that window: the port belongs to the relay from
//! the moment it is chosen until the relay exits.

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Loopback with an OS-assigned port; the relay reports the port it received.
pub const EPHEMERAL_LOOPBACK_BIND: &str = "127.0.0.1:0";

/// Startup budget for a debug-build relay on a loaded CI runner. Startup only
/// parses configuration, reads TLS files and binds, so this bounds a hang
/// rather than pacing the test.
pub const RELAY_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Message of the startup diagnostic `serve_with_transport` emits after the
/// listener is bound (and therefore already accepting into its backlog).
const LISTENING_MESSAGE: &str = "clipsync-relay listening";

/// Field of that diagnostic carrying the bound socket address.
const ADDRESS_FIELD: &str = "addr=";

/// A relay child process and the address it reported after binding.
pub struct Relay {
    pub child: Child,
    pub address: SocketAddr,
}

impl Relay {
    /// Spawn `command` and wait until the relay reports its bound address.
    ///
    /// Panics with the relay's stderr if it exits first, so a configuration
    /// regression is reported instead of a connection error.
    pub async fn spawn(mut command: Command) -> Self {
        // Pin the diagnostic format independently of the caller's environment:
        // an inherited RUST_LOG could filter the line out, and colour escapes
        // would split the field from its value.
        command
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .stdout(Stdio::piped());
        let mut child = command.spawn().expect("spawn relay binary");
        let mut lines = BufReader::new(child.stdout.take().expect("piped relay stdout")).lines();
        let reported = tokio::time::timeout(RELAY_STARTUP_TIMEOUT, async {
            while let Some(line) = lines.next_line().await.expect("read relay stdout") {
                if let Some(address) = listening_address(&line) {
                    return Some(address);
                }
            }
            None
        })
        .await
        .expect("relay did not report a listening address in time");
        let Some(address) = reported else {
            let output = child.wait_with_output().await.expect("collect relay exit");
            panic!(
                "relay exited before listening ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        };
        // Keep draining diagnostics so a chatty relay never blocks on a full
        // stdout pipe while the test is still talking to it.
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Self { child, address }
    }

    pub async fn stop(mut self) {
        self.child.kill().await.expect("stop relay");
        self.child.wait().await.expect("reap relay");
    }
}

fn listening_address(line: &str) -> Option<SocketAddr> {
    if !line.contains(LISTENING_MESSAGE) {
        return None;
    }
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix(ADDRESS_FIELD))
        .unwrap_or_else(|| panic!("listening diagnostic without {ADDRESS_FIELD}: {line}"));
    Some(
        value
            .parse()
            .unwrap_or_else(|error| panic!("bad listening address {value:?}: {error}")),
    )
}
