// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! Desktop pairing and daemon control shared by the window.

use std::future::Future;
use std::pin::Pin;

use clipsync_core::{begin_create_room, join_room, App, SasCheck, SasDecision};
use clipsync_daemon::install_and_start;
use clipsync_storage::LocalStore;

/// A verification code the window must show next to Confirm and Cancel
/// buttons. Dropping it unanswered rejects the pairing.
#[derive(Debug)]
pub struct SasPrompt {
    pub check: SasCheck,
    reply: tokio::sync::oneshot::Sender<SasDecision>,
}

impl SasPrompt {
    pub fn answer(self, decision: SasDecision) {
        let _ = self.reply.send(decision);
    }

    /// The pairing behind this prompt already ended (timeout, peer
    /// rejection, relay error); the window should stop showing it.
    pub fn is_closed(&self) -> bool {
        self.reply.is_closed()
    }
}

pub type SasConfirm =
    Box<dyn FnOnce(SasCheck) -> Pin<Box<dyn Future<Output = SasDecision> + Send>> + Send>;

/// Forward the SAS check to the window through `show` and wait for the
/// user's answer. A window that drops the prompt rejects the pairing.
pub fn window_confirmer(show: impl FnOnce(SasPrompt) + Send + 'static) -> SasConfirm {
    Box::new(move |check: SasCheck| {
        Box::pin(async move {
            let (reply, answer) = tokio::sync::oneshot::channel();
            show(SasPrompt { check, reply });
            answer.await.unwrap_or(SasDecision::Rejected)
        })
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeskStatus {
    pub device_id: String,
    pub room_id: Option<String>,
    pub relay_url: String,
    pub daemon: bool,
    pub connected: bool,
}

pub fn open_app(relay_url: &str) -> Result<App, String> {
    let app = App::open().map_err(|e| e.to_string())?;
    app.init(Some(relay_url.to_string()))
        .map_err(|e| e.to_string())?;
    Ok(app)
}

pub fn local_status(app: &App) -> Result<DeskStatus, String> {
    let id = app.identity().map_err(|e| e.to_string())?;
    let cfg = app.store.load_config().map_err(|e| e.to_string())?;
    let room = app.store.current_room().ok();
    Ok(DeskStatus {
        device_id: id.device_id.to_string(),
        room_id: room.map(|r| r.room_id.to_string()),
        relay_url: cfg.relay_url,
        daemon: false,
        connected: false,
    })
}

pub fn normalize_join_code(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() == 6 {
        Some(digits)
    } else {
        None
    }
}

/// Create a room and return its code. Pairing continues in the background
/// and commits only after `confirm` accepts the verification code.
pub async fn create_room(store: LocalStore, confirm: SasConfirm) -> Result<String, String> {
    let app = App {
        store: store.clone(),
    };
    let identity = app.identity().map_err(|e| e.to_string())?;
    let cfg = app.store.load_config().map_err(|e| e.to_string())?;
    let pending = begin_create_room(&identity, &cfg, 120)
        .await
        .map_err(|e| e.to_string())?;
    let code = pending.offer.pairing_code.clone();
    tokio::spawn(async move {
        if pending.wait(&store, &identity, confirm).await.is_ok() {
            let _ = install_and_start(&store.paths);
        }
    });
    Ok(code)
}

pub async fn join_room_code(app: &App, code: &str, confirm: SasConfirm) -> Result<String, String> {
    let code = normalize_join_code(code).ok_or_else(|| "enter a 6-digit code".to_string())?;
    let identity = app.identity().map_err(|e| e.to_string())?;
    let cfg = app.store.load_config().map_err(|e| e.to_string())?;
    join_room(&app.store, &identity, &cfg, &code, confirm)
        .await
        .map_err(|e| e.to_string())?;
    let room = app.store.current_room().map_err(|e| e.to_string())?;
    install_and_start(&app.store.paths).map_err(|e| e.to_string())?;
    Ok(room.room_id.to_string())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn window_confirmer_forwards_the_check_and_rejects_unanswered_prompts() {
        let check = SasCheck {
            sas: "482913".into(),
            peer_device_id: "peer".into(),
            peer_device_name: "Laptop".into(),
        };
        for answer in [
            Some(SasDecision::Confirmed),
            Some(SasDecision::Rejected),
            None,
        ] {
            let (shown_tx, shown) = std::sync::mpsc::channel();
            let confirm = window_confirmer(move |prompt| shown_tx.send(prompt).unwrap());
            let decision = tokio::spawn(confirm(check.clone()));
            let prompt = tokio::task::spawn_blocking(move || shown.recv().unwrap())
                .await
                .unwrap();
            assert_eq!(prompt.check, check);
            match answer {
                Some(decision) => prompt.answer(decision),
                None => drop(prompt),
            }
            assert_eq!(
                decision.await.unwrap(),
                answer.unwrap_or(SasDecision::Rejected)
            );
        }
    }

    #[test]
    fn join_code_keeps_six_digits() {
        assert_eq!(normalize_join_code("48 29-13").as_deref(), Some("482913"));
        assert!(normalize_join_code("12345").is_none());
        assert!(normalize_join_code("1234567").is_none());
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod command_tests {
    use super::*;
    use clipsync_crypto::DeviceIdentity;
    use clipsync_storage::{AppConfig, AppPaths};

    fn accept() -> SasConfirm {
        Box::new(|_| Box::pin(async { SasDecision::Confirmed }))
    }

    /// Confirmers for two windows whose users compare the codes they see.
    /// Only the Unix-only full pairing test uses it.
    #[cfg(unix)]
    fn sas_comparison() -> (SasConfirm, SasConfirm) {
        fn side(
            mine: tokio::sync::oneshot::Sender<String>,
            theirs: tokio::sync::oneshot::Receiver<String>,
        ) -> SasConfirm {
            Box::new(move |check: SasCheck| {
                Box::pin(async move {
                    let _ = mine.send(check.sas.clone());
                    match theirs.await {
                        Ok(other) if check.matches(&other) => SasDecision::Confirmed,
                        _ => SasDecision::Rejected,
                    }
                })
            })
        }
        let (creator_tx, creator_rx) = tokio::sync::oneshot::channel();
        let (joiner_tx, joiner_rx) = tokio::sync::oneshot::channel();
        (side(creator_tx, joiner_rx), side(joiner_tx, creator_rx))
    }

    fn fixture() -> (tempfile::TempDir, App) {
        let root = tempfile::tempdir().unwrap();
        let app = App::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap();
        let id = DeviceIdentity::from_bytes("desktop-command-device".into(), [9; 32]);
        std::fs::write(
            app.store.paths.identity_file(),
            serde_json::to_vec(&id.to_stored()).unwrap(),
        )
        .unwrap();
        app.store
            .save_config(&AppConfig {
                relay_url: "http://127.0.0.1:0".into(),
                ..Default::default()
            })
            .unwrap();
        (root, app)
    }

    #[test]
    fn local_status_reports_identity_and_selected_room() {
        let (_root, app) = fixture();
        let status = local_status(&app).unwrap();
        assert_eq!(status.device_id, "desktop-command-device");
        assert!(status.room_id.is_none() && !status.daemon && !status.connected);
        let room = serde_json::from_value(serde_json::json!({
            "room_id":"desktop-room", "epoch":1, "role":"owner",
            "created_at":"2026-11-15T00:00:00Z", "device_token":"synthetic",
            "peers":[]
        }))
        .unwrap();
        app.store.upsert_room(room).unwrap();
        assert_eq!(
            local_status(&app).unwrap().room_id.as_deref(),
            Some("desktop-room")
        );
        std::fs::write(app.store.paths.config_file(), "broken = [").unwrap();
        assert!(local_status(&app).unwrap_err().contains("toml"));
    }

    #[tokio::test]
    async fn commands_reject_invalid_codes_and_unavailable_relay_without_launching_services() {
        let (_root, app) = fixture();
        assert_eq!(
            join_room_code(&app, "123", accept()).await.unwrap_err(),
            "enter a 6-digit code"
        );
        assert!(join_room_code(&app, "482913", accept())
            .await
            .unwrap_err()
            .contains("relay join"));
        assert!(create_room(app.store.clone(), accept())
            .await
            .unwrap_err()
            .contains("relay create"));
        std::fs::write(app.store.paths.config_file(), "broken = [").unwrap();
        assert!(create_room(app.store.clone(), accept())
            .await
            .unwrap_err()
            .contains("toml"));
        assert!(join_room_code(&app, "482913", accept())
            .await
            .unwrap_err()
            .contains("toml"));
    }

    #[tokio::test]
    async fn create_returns_live_relay_offer_before_the_other_device_joins() {
        let (_root, app) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = clipsync_relay::RelayConfig {
            bind: address,
            public_url: format!("http://{address}"),
            token_secret: vec![3; 32],
            webhook_url: None,
            webhook_secret: None,
            metrics_token: None,
        };
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                clipsync_relay::router(config)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let mut cfg = app.store.load_config().unwrap();
        cfg.relay_url = format!("http://{address}");
        app.store.save_config(&cfg).unwrap();
        let code = create_room(app.store.clone(), accept()).await.unwrap();
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
        assert!(
            app.store.current_room().is_err(),
            "an offer must not imply completed pairing"
        );
        server.abort();
    }

    #[cfg(unix)]
    #[test]
    fn desktop_pairing_commits_both_peer_records_and_installs_only_isolated_services() {
        const CHILD: &str = "CLIPSYNC_TEST_DESKTOP_PAIRING_CHILD";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let tools = root.path().join("tools");
            std::fs::create_dir(&tools).unwrap();
            for name in ["systemctl", "launchctl"] {
                let path = tools.join(name);
                std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let mut paths = vec![tools];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "command_tests::desktop_pairing_commits_both_peer_records_and_installs_only_isolated_services", "--nocapture"])
                .env(CHILD, "1").env("HOME", root.path()).env("USERPROFILE", root.path())
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env_remove("CLIPSYNC_LISTEN").stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                if std::time::Instant::now() > deadline {
                    child.kill().unwrap();
                    panic!("desktop pairing worker exceeded its deadline");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let (_first_home, first) = fixture();
            let (_second_home, second) = fixture();
            let identity = DeviceIdentity::from_bytes("desktop-joining-device".into(), [10; 32]);
            std::fs::write(
                second.store.paths.identity_file(),
                serde_json::to_vec(&identity.to_stored()).unwrap(),
            )
            .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(
                    listener,
                    clipsync_relay::router(clipsync_relay::RelayConfig {
                        bind: address,
                        public_url: format!("http://{address}"),
                        token_secret: vec![13; 32],
                        webhook_url: None,
                        webhook_secret: None,
                        metrics_token: None,
                    })
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            for app in [&first, &second] {
                let mut cfg = app.store.load_config().unwrap();
                cfg.relay_url = format!("http://{address}");
                app.store.save_config(&cfg).unwrap();
            }
            let (creator_sas, joiner_sas) = sas_comparison();
            let code = create_room(first.store.clone(), creator_sas).await.unwrap();
            let formatted = format!("{} {}-{}", &code[..2], &code[2..4], &code[4..]);
            let room = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                join_room_code(&second, &formatted, joiner_sas),
            )
            .await
            .unwrap()
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while first.store.current_room().is_err() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(first.store.current_room().unwrap().room_id.as_str(), room);
            assert_eq!(
                local_status(&second).unwrap().room_id.as_deref(),
                Some(room.as_str())
            );
            assert!(clipsync_daemon::is_service_installed());
            let first_peer = &first.store.current_room().unwrap().peers[0];
            assert_eq!(first_peer.device_id, identity.device_id);
            server.abort();
        });
    }
}
