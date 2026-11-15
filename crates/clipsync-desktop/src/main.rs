// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! ClipSync window for macOS, Windows, and Linux.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use clipsync_core::App;
use clipsync_daemon::connect_ipc;
use clipsync_desktop::{create_room, join_room_code, local_status, open_app, DeskStatus};
use eframe::egui;

const INK: egui::Color32 = egui::Color32::from_rgb(18, 20, 22);
const PANEL: egui::Color32 = egui::Color32::from_rgb(30, 33, 36);
const MINT: egui::Color32 = egui::Color32::from_rgb(79, 227, 194);

enum Work {
    Created(Result<String, String>),
    Joined(Result<String, String>),
}

struct DesktopApp {
    core: App,
    rt: tokio::runtime::Runtime,
    tx: Sender<Work>,
    rx: Receiver<Work>,
    status: DeskStatus,
    pairing_code: Option<String>,
    join_code: String,
    error: Option<String>,
    busy: bool,
}

impl DesktopApp {
    fn new() -> Result<Self, String> {
        let relay = std::env::var("CLIPSYNC_RELAY_URL")
            .unwrap_or_else(|_| "https://clipsync.develicit.dev".into());
        let core = open_app(&relay)?;
        let status = local_status(&core)?;
        let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel();
        Ok(Self {
            core,
            rt,
            tx,
            rx,
            status,
            pairing_code: None,
            join_code: String::new(),
            error: None,
            busy: false,
        })
    }

    fn refresh(&mut self) {
        if let Ok(local) = local_status(&self.core) {
            self.status.device_id = local.device_id;
            self.status.room_id = local.room_id.clone();
            self.status.relay_url = local.relay_url;
        }
        let paths = self.core.store.paths.clone();
        if let Ok(mut ipc) = self.rt.block_on(connect_ipc(&paths)) {
            if let Ok(resp) = self
                .rt
                .block_on(ipc.request(&clipsync_daemon::IpcRequest::Status))
            {
                if let Some(st) = resp.status {
                    self.status.daemon = st.daemon;
                    self.status.connected = st.connected;
                    if let Some(room) = st.room_id {
                        self.status.room_id = Some(room.to_string());
                    }
                }
            }
        } else {
            self.status.daemon = false;
            self.status.connected = false;
        }
    }

    fn start_create(&mut self) {
        self.busy = true;
        self.error = None;
        let store = self.core.store.clone();
        let tx = self.tx.clone();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            let result = handle.block_on(create_room(store));
            let _ = tx.send(Work::Created(result));
        });
    }

    fn start_join(&mut self) {
        self.busy = true;
        self.error = None;
        let app = self.core.clone();
        let code = self.join_code.clone();
        let tx = self.tx.clone();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            let result = handle.block_on(join_room_code(&app, &code));
            let _ = tx.send(Work::Joined(result));
        });
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.busy = false;
            match msg {
                Work::Created(Ok(code)) => {
                    self.pairing_code = Some(code);
                    self.error = None;
                }
                Work::Created(Err(e)) | Work::Joined(Err(e)) => self.error = Some(e),
                Work::Joined(Ok(room)) => {
                    self.status.room_id = Some(room);
                    self.pairing_code = None;
                    self.error = None;
                }
            }
        }
    }
}

impl DesktopApp {
    fn update_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.drain();
        ctx.request_repaint_after(Duration::from_secs(1));
        if ctx.input(|i| i.time).fract() < 0.05 {
            self.refresh();
        }

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = INK;
        visuals.window_fill = INK;
        visuals.extreme_bg_color = PANEL;
        ctx.set_visuals(visuals);

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("CLIPSYNC")
                    .color(MINT)
                    .monospace()
                    .size(13.0),
            );
            ui.label(egui::RichText::new("Desktop").size(28.0).strong());
            ui.add_space(8.0);
            let line = match (&self.status.room_id, self.status.connected) {
                (Some(room), true) => format!("Paired · {}… · relay connected", &room[..room.len().min(8)]),
                (Some(room), false) => format!("Paired · {}… · daemon {}", &room[..room.len().min(8)], if self.status.daemon { "up" } else { "off" }),
                (None, _) => "Not paired".into(),
            };
            ui.label(egui::RichText::new(line).monospace().weak());
            ui.label(
                egui::RichText::new(format!("device {}", &self.status.device_id[..self.status.device_id.len().min(12)]))
                    .monospace()
                    .weak(),
            );
            ui.add_space(16.0);

            if let Some(code) = &self.pairing_code {
                if self.status.room_id.is_none() {
                    egui::Frame::NONE
                        .fill(PANEL)
                        .corner_radius(16.0)
                        .inner_margin(16.0)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("PAIRING CODE").color(MINT).monospace());
                            ui.label(egui::RichText::new(code).size(40.0).monospace().strong());
                            ui.label("Enter this code on the other device.");
                        });
                    ui.add_space(12.0);
                }
            }

            if self.status.room_id.is_none() {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Create room").min_size(egui::vec2(180.0, 36.0)))
                    .clicked()
                {
                    self.start_create();
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.join_code)
                            .hint_text("Join code")
                            .desired_width(160.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Join").min_size(egui::vec2(80.0, 32.0)))
                        .clicked()
                    {
                        self.start_join();
                    }
                });
            } else {
                ui.label("Clipboard sync runs in the login daemon. Copy on this computer or a paired device.");
                if !self.status.daemon && ui.button("Start background sync").clicked() {
                    if let Err(e) = clipsync_daemon::install_and_start(&self.core.store.paths) {
                        self.error = Some(e.to_string());
                    }
                }
            }

            if let Some(err) = &self.error {
                ui.add_space(12.0);
                ui.colored_label(egui::Color32::from_rgb(255, 115, 102), err);
            }
        });
    }
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_ui(ui);
    }
}

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_decorations(true)
            .with_inner_size([440.0, 560.0])
            .with_min_inner_size([380.0, 480.0]),
        // Preserve the existing default renderer across eframe's default change.
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    }
}

fn main() -> eframe::Result {
    let app = match DesktopApp::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("clipsync-desktop: {e}");
            std::process::exit(1);
        }
    };
    let options = native_options();
    eframe::run_native("ClipSync", options, Box::new(|_cc| Ok(Box::new(app))))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use clipsync_crypto::DeviceIdentity;
    use clipsync_daemon::{
        read_request, write_response, IpcRequest, IpcResponse, IpcServer, StatusPayload,
    };
    use clipsync_storage::{AppConfig, AppPaths};

    fn fixture() -> (tempfile::TempDir, DesktopApp) {
        let root = tempfile::tempdir().unwrap();
        let core = App::open_from(AppPaths::from_root(root.path().to_path_buf())).unwrap();
        let id = DeviceIdentity::from_bytes("desktop-test-device".into(), [7; 32]);
        std::fs::write(
            core.store.paths.identity_file(),
            serde_json::to_vec(&id.to_stored()).unwrap(),
        )
        .unwrap();
        let cfg = AppConfig {
            relay_url: "http://127.0.0.1:0".into(),
            ..Default::default()
        };
        core.store.save_config(&cfg).unwrap();
        let status = local_status(&core).unwrap();
        let (tx, rx) = mpsc::channel();
        let app = DesktopApp {
            core,
            rt: tokio::runtime::Runtime::new().unwrap(),
            tx,
            rx,
            status,
            pairing_code: None,
            join_code: String::new(),
            error: None,
            busy: false,
        };
        (root, app)
    }

    #[test]
    fn native_window_keeps_title_decorations_and_glow_renderer() {
        let options = native_options();
        assert_eq!(options.viewport.title.as_deref(), Some("ClipSync"));
        assert_eq!(options.viewport.decorations, Some(true));
        assert_eq!(options.viewport.inner_size, Some(egui::vec2(440.0, 560.0)));
        assert_eq!(
            options.viewport.min_inner_size,
            Some(egui::vec2(380.0, 480.0))
        );
        assert_eq!(options.renderer, eframe::Renderer::Glow);
    }

    fn frame(app: &mut DesktopApp) -> String {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(0.5),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(440.0, 560.0),
                )),
                ..Default::default()
            },
            |ui| app.update_ui(ui),
        );
        // This text-only harness has no renderer to upload the font atlas to.
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text.galley.text().to_owned())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn headless_window_shows_pairing_and_connection_states() {
        let (_root, mut app) = fixture();
        let text = frame(&mut app);
        assert!(text.contains("Not paired"));
        assert!(text.contains("Create room"));
        assert!(text.contains("Join"));
        assert!(text.contains("device desktop-test"));
        app.pairing_code = Some("482913".into());
        app.busy = true;
        app.error = Some("synthetic relay failure".into());
        let text = frame(&mut app);
        assert!(text.contains("PAIRING CODE") && text.contains("482913"));
        assert!(text.contains("synthetic relay failure"));
        app.status.room_id = Some("01J00000000000000000000000".into());
        app.status.connected = true;
        let text = frame(&mut app);
        assert!(text.contains("relay connected"));
        assert!(!text.contains("PAIRING CODE"));
        app.status.connected = false;
        app.status.daemon = true;
        assert!(frame(&mut app).contains("daemon up"));
        app.status.daemon = false;
        let text = frame(&mut app);
        assert!(text.contains("daemon off"));
        assert!(text.contains("Start background sync"));
    }

    #[test]
    fn worker_results_clear_busy_and_replace_pairing_or_room_state() {
        let (_root, mut app) = fixture();
        app.busy = true;
        app.error = Some("previous error".into());
        app.tx.send(Work::Created(Ok("482913".into()))).unwrap();
        app.drain();
        assert!(!app.busy && app.error.is_none());
        assert_eq!(app.pairing_code.as_deref(), Some("482913"));
        app.tx
            .send(Work::Created(Err("create rejected".into())))
            .unwrap();
        app.drain();
        assert_eq!(app.error.as_deref(), Some("create rejected"));
        app.tx
            .send(Work::Joined(Err("join rejected".into())))
            .unwrap();
        app.drain();
        assert_eq!(app.error.as_deref(), Some("join rejected"));
        app.tx.send(Work::Joined(Ok("paired-room".into()))).unwrap();
        app.drain();
        assert_eq!(app.status.room_id.as_deref(), Some("paired-room"));
        assert!(app.pairing_code.is_none() && app.error.is_none());
    }

    #[test]
    fn invalid_join_and_broken_configuration_surface_background_errors() {
        let (_root, mut app) = fixture();
        app.join_code = "12".into();
        app.error = Some("old error".into());
        app.start_join();
        assert!(app.busy && app.error.is_none());
        let work = app.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        app.tx.send(work).unwrap();
        app.drain();
        assert_eq!(app.error.as_deref(), Some("enter a 6-digit code"));
        std::fs::write(app.core.store.paths.config_file(), "broken = [").unwrap();
        app.start_create();
        assert!(app.busy && app.error.is_none());
        let work = app.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        app.tx.send(work).unwrap();
        app.drain();
        assert!(!app.busy);
        assert!(app.error.as_deref().unwrap().contains("toml"));
    }

    #[test]
    fn refresh_uses_ipc_status_and_resets_disconnected_daemon() {
        // Endpoint overrides belong to the embedding process. Clear them only
        // in an isolated child, so this fixture can never unlink a user socket.
        const CHILD: &str = "CLIPSYNC_TEST_DESKTOP_IPC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::refresh_uses_ipc_status_and_resets_disconnected_daemon",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env_remove(clipsync_daemon::LISTEN_ENV)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let (_root, mut app) = fixture();
        let server = app
            .rt
            .block_on(IpcServer::bind(&app.core.store.paths))
            .unwrap();
        let worker = app.rt.spawn(async move {
            let mut conn = server.accept().await.unwrap();
            assert!(matches!(
                read_request(&mut conn).await.unwrap(),
                Some(IpcRequest::Status)
            ));
            write_response(
                &mut conn,
                &IpcResponse {
                    ok: true,
                    error: None,
                    pull: None,
                    status: Some(StatusPayload {
                        daemon: true,
                        paused: false,
                        connected: true,
                        peer_online: true,
                        room_id: Some("daemon-room".into()),
                        device_id: None,
                        device_name: None,
                        relay_url: None,
                        last_item_kind: None,
                        last_sync_at: None,
                    }),
                },
            )
            .await
            .unwrap();
        });
        app.refresh();
        app.rt.block_on(worker).unwrap();
        assert!(app.status.daemon && app.status.connected);
        assert_eq!(app.status.room_id.as_deref(), Some("daemon-room"));
        assert_eq!(app.status.relay_url, "http://127.0.0.1:0");
        app.refresh();
        assert!(!app.status.daemon && !app.status.connected);
        assert!(app.status.room_id.is_none());
    }

    #[test]
    fn constructor_respects_selected_home_and_reports_configuration_errors() {
        let (root, _app) = fixture();
        for invalid in [false, true] {
            if invalid {
                std::fs::write(root.path().join("config/config.toml"), "broken = [").unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::constructor_child", "--nocapture"])
                .env(
                    "CLIPSYNC_TEST_DESKTOP_CASE",
                    if invalid { "invalid" } else { "valid" },
                )
                .env("CLIPSYNC_HOME", root.path())
                .env("CLIPSYNC_RELAY_URL", "http://127.0.0.1:19")
                .env_remove("CLIPSYNC_LISTEN")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn constructor_child() {
        let Ok(case) = std::env::var("CLIPSYNC_TEST_DESKTOP_CASE") else {
            return;
        };
        if case == "invalid" {
            assert!(DesktopApp::new().err().unwrap().contains("toml"));
        } else {
            let app = DesktopApp::new().unwrap();
            assert_eq!(app.status.device_id, "desktop-test-device");
            assert_eq!(app.status.relay_url, "http://127.0.0.1:19");
            assert!(app.status.room_id.is_none());
        }
    }
}
