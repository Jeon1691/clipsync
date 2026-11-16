// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
#![cfg_attr(all(coverage_nightly, test), feature(coverage_attribute))]
//! ClipSync window for macOS, Windows, and Linux.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use clipsync_core::{App, SasDecision};
use clipsync_daemon::connect_ipc;
use clipsync_desktop::{
    create_room, join_room_code, local_status, open_app, window_confirmer, DeskStatus, SasPrompt,
};
use eframe::egui;

const INK: egui::Color32 = egui::Color32::from_rgb(18, 20, 22);
const PANEL: egui::Color32 = egui::Color32::from_rgb(30, 33, 36);
const MINT: egui::Color32 = egui::Color32::from_rgb(79, 227, 194);

enum Work {
    Created(Result<String, String>),
    Joined(Result<String, String>),
    /// Pairing reached key confirmation; the user must compare codes.
    Verify(SasPrompt),
}

const SAS_CONFIRM_LABEL: &str = "Codes match";
const SAS_CANCEL_LABEL: &str = "Cancel";
const SAS_REJECTED_MESSAGE: &str =
    "Pairing cancelled: the verification codes were not confirmed. Nothing was stored.";
const SAS_EXPIRED_MESSAGE: &str =
    "Pairing ended before the verification codes were confirmed. Nothing was stored.";

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
    sas_prompt: Option<SasPrompt>,
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
            sas_prompt: None,
        })
    }

    fn confirmer(&self) -> clipsync_desktop::SasConfirm {
        let tx = self.tx.clone();
        window_confirmer(move |prompt| {
            let _ = tx.send(Work::Verify(prompt));
        })
    }

    fn answer_sas(&mut self, decision: SasDecision) {
        if let Some(prompt) = self.sas_prompt.take() {
            prompt.answer(decision);
        }
        if decision == SasDecision::Rejected {
            self.pairing_code = None;
            self.error = Some(SAS_REJECTED_MESSAGE.into());
        }
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
        let confirm = self.confirmer();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            let result = handle.block_on(create_room(store, confirm));
            let _ = tx.send(Work::Created(result));
        });
    }

    fn start_join(&mut self) {
        self.busy = true;
        self.error = None;
        let app = self.core.clone();
        let code = self.join_code.clone();
        let tx = self.tx.clone();
        let confirm = self.confirmer();
        let handle = self.rt.handle().clone();
        std::thread::spawn(move || {
            let result = handle.block_on(join_room_code(&app, &code, confirm));
            let _ = tx.send(Work::Joined(result));
        });
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            if let Work::Verify(prompt) = msg {
                // A join keeps its busy state until the pairing finishes.
                self.sas_prompt = Some(prompt);
                continue;
            }
            self.busy = false;
            match msg {
                Work::Verify(_) => {}
                Work::Created(Ok(code)) => {
                    self.pairing_code = Some(code);
                    self.error = None;
                }
                Work::Created(Err(e)) | Work::Joined(Err(e)) => {
                    // The pairing behind any open prompt is over.
                    self.sas_prompt = None;
                    self.error = Some(e);
                }
                Work::Joined(Ok(room)) => {
                    self.sas_prompt = None;
                    self.status.room_id = Some(room);
                    self.pairing_code = None;
                    self.error = None;
                }
            }
        }
        if self.sas_prompt.as_ref().is_some_and(SasPrompt::is_closed) {
            // A background create timed out or the relay aborted it.
            self.sas_prompt = None;
            self.pairing_code = None;
            if self.error.is_none() {
                self.error = Some(SAS_EXPIRED_MESSAGE.into());
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

            if let Some(prompt) = &self.sas_prompt {
                let peer = if prompt.check.peer_device_name.is_empty() {
                    "the other device"
                } else {
                    prompt.check.peer_device_name.as_str()
                };
                let mut decision = None;
                egui::Frame::NONE
                    .fill(PANEL)
                    .corner_radius(16.0)
                    .inner_margin(16.0)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("VERIFICATION CODE").color(MINT).monospace());
                        ui.label(egui::RichText::new(prompt.check.display()).size(40.0).monospace().strong());
                        ui.label(format!("Check that {peer} shows the same code."));
                        ui.label("Confirm only if both devices show the same code.");
                        ui.horizontal(|ui| {
                            if ui.button(SAS_CONFIRM_LABEL).clicked() {
                                decision = Some(SasDecision::Confirmed);
                            }
                            if ui.button(SAS_CANCEL_LABEL).clicked() {
                                decision = Some(SasDecision::Rejected);
                            }
                        });
                    });
                if let Some(decision) = decision {
                    self.answer_sas(decision);
                }
                ui.add_space(12.0);
            }

            if let Some(code) = &self.pairing_code {
                if self.status.room_id.is_none() && self.sas_prompt.is_none() {
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
            sas_prompt: None,
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

    #[derive(Default)]
    struct HeadlessUi {
        ctx: egui::Context,
    }

    impl HeadlessUi {
        fn paint(
            &self,
            app: &mut DesktopApp,
            time: f64,
            events: Vec<egui::Event>,
        ) -> Vec<(String, egui::Rect)> {
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(440.0, 560.0),
                    )),
                    events,
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
                        Some((
                            text.galley.text().to_owned(),
                            egui::Rect::from_min_size(text.pos, text.galley.size()),
                        ))
                    } else {
                        None
                    }
                })
                .collect()
        }

        fn text(&self, app: &mut DesktopApp) -> String {
            self.paint(app, 0.5, Vec::new())
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn click(&self, app: &mut DesktopApp, label: &str) {
            let pos = self
                .paint(app, 0.5, Vec::new())
                .into_iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("missing control {label}"))
                .1
                .center();
            for pressed in [true, false] {
                self.paint(
                    app,
                    0.5,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
    }

    fn frame(app: &mut DesktopApp) -> String {
        HeadlessUi::default().text(app)
    }

    fn child_output(command: &mut std::process::Command) -> std::process::Output {
        let mut child = command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "desktop child exceeded 10 seconds: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn headless_controls_edit_join_code_and_display_worker_errors() {
        let (_root, mut app) = fixture();
        let ui = HeadlessUi::default();
        ui.click(&mut app, "Join code");
        ui.paint(&mut app, 0.5, vec![egui::Event::Text("12".into())]);
        assert_eq!(app.join_code, "12");
        app.error = Some("old error".into());
        ui.click(&mut app, "Join");
        assert!(app.busy && app.error.is_none());
        let work = app.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        app.tx.send(work).unwrap();
        assert!(ui.text(&mut app).contains("enter a 6-digit code"));
        assert!(!app.busy);
        assert_eq!(app.join_code, "12");

        std::fs::write(app.core.store.paths.config_file(), "broken = [").unwrap();
        ui.click(&mut app, "Create room");
        assert!(app.busy && app.error.is_none());
        let work = app.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        app.tx.send(work).unwrap();
        assert!(ui.text(&mut app).contains("toml"));
        assert!(!app.busy);
    }

    #[test]
    fn headless_pending_controls_ignore_clicks_and_render_completed_work() {
        let (_root, mut app) = fixture();
        let ui = HeadlessUi::default();
        app.busy = true;
        app.error = Some("previous error".into());
        ui.click(&mut app, "Create room");
        ui.click(&mut app, "Join");
        assert!(app.busy);
        assert_eq!(app.error.as_deref(), Some("previous error"));
        assert!(matches!(app.rx.try_recv(), Err(mpsc::TryRecvError::Empty)));

        app.tx.send(Work::Created(Ok("482913".into()))).unwrap();
        let text = ui.text(&mut app);
        assert!(!app.busy && app.error.is_none());
        assert!(text.contains("PAIRING CODE") && text.contains("482913"));
        assert!(text.contains("Create room") && text.contains("Join"));

        app.busy = true;
        app.error = Some("previous join error".into());
        app.tx.send(Work::Joined(Ok("paired-room".into()))).unwrap();
        let text = ui.text(&mut app);
        assert!(!app.busy && app.error.is_none());
        assert!(text.contains("Paired · paired-r… · daemon off"));
        assert!(text.contains("Start background sync"));
        assert!(!text.contains("PAIRING CODE") && !text.contains("Create room"));
    }

    fn sas_prompt(app: &DesktopApp) -> tokio::sync::oneshot::Receiver<SasDecision> {
        let (answer_tx, answer) = tokio::sync::oneshot::channel();
        let confirm = app.confirmer();
        app.rt.spawn(async move {
            let decision = confirm(clipsync_core::SasCheck {
                sas: "482913".into(),
                peer_device_id: "peer".into(),
                peer_device_name: "Work laptop".into(),
            })
            .await;
            let _ = answer_tx.send(decision);
        });
        answer
    }

    #[test]
    fn headless_sas_prompt_shows_code_and_peer_and_reports_the_answer() {
        for (label, expected) in [
            (SAS_CONFIRM_LABEL, SasDecision::Confirmed),
            (SAS_CANCEL_LABEL, SasDecision::Rejected),
        ] {
            let (_root, mut app) = fixture();
            let ui = HeadlessUi::default();
            app.pairing_code = Some("135790".into());
            app.busy = true;
            let answer = sas_prompt(&app);
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app.sas_prompt.is_none() {
                assert!(std::time::Instant::now() < deadline, "prompt never arrived");
                app.drain();
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                app.busy,
                "a verification prompt must not end a pending join"
            );
            let text = ui.text(&mut app);
            assert!(text.contains("VERIFICATION CODE") && text.contains("482 913"));
            assert!(text.contains("Check that Work laptop shows the same code."));
            assert!(
                !text.contains("PAIRING CODE"),
                "the SAS replaces the pairing code"
            );
            ui.click(&mut app, label);
            assert_eq!(app.rt.block_on(answer).unwrap(), expected);
            assert!(app.sas_prompt.is_none());
            let text = ui.text(&mut app);
            assert!(!text.contains("VERIFICATION CODE"));
            if expected == SasDecision::Rejected {
                assert!(app.pairing_code.is_none());
                assert!(text.contains("Nothing was stored"));
            } else {
                assert_eq!(app.pairing_code.as_deref(), Some("135790"));
                assert!(app.error.is_none());
            }
        }
    }

    #[test]
    fn prompts_close_when_their_pairing_ends_without_an_answer() {
        let (_root, mut app) = fixture();
        // The pairing (here, its decision future) ends before the user answers.
        let (reply, answer) = tokio::sync::oneshot::channel::<SasDecision>();
        let confirm = app.confirmer();
        let pending = app.rt.spawn(async move {
            let _ = reply.send(
                confirm(clipsync_core::SasCheck {
                    sas: "482913".into(),
                    peer_device_id: "peer".into(),
                    peer_device_name: "Work laptop".into(),
                })
                .await,
            );
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while app.sas_prompt.is_none() {
            assert!(std::time::Instant::now() < deadline, "prompt never arrived");
            app.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        app.pairing_code = Some("135790".into());
        pending.abort();
        let _ = app.rt.block_on(pending);
        drop(answer);
        app.drain();
        assert!(app.sas_prompt.is_none() && app.pairing_code.is_none());
        let text = frame(&mut app);
        assert!(!text.contains("VERIFICATION CODE"));
        assert!(text.contains("Pairing ended before the verification codes were confirmed"));

        // A join result also retires any prompt still on screen.
        let _answer = sas_prompt(&app);
        while app.sas_prompt.is_none() {
            app.drain();
            std::thread::sleep(Duration::from_millis(5));
        }
        app.tx
            .send(Work::Joined(Err("pairing failed".into())))
            .unwrap();
        app.drain();
        assert!(app.sas_prompt.is_none());
        assert_eq!(app.error.as_deref(), Some("pairing failed"));
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
            let output = child_output(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "tests::refresh_uses_ipc_status_and_resets_disconnected_daemon",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .env_remove(clipsync_daemon::LISTEN_ENV),
            );
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

        for case in [
            "missing-status",
            "missing-room",
            "malformed",
            "broken-config",
        ] {
            let (_root, mut app) = fixture();
            if case == "broken-config" {
                std::fs::write(app.core.store.paths.config_file(), "broken = [").unwrap();
                app.status.device_id = "cached-device".into();
                app.status.relay_url = "cached-relay".into();
                app.status.room_id = Some("cached-room".into());
            }
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
                if case == "malformed" {
                    use tokio::io::AsyncWriteExt;
                    conn.write_all(b"invalid response\n").await.unwrap();
                } else {
                    write_response(
                        &mut conn,
                        &IpcResponse {
                            ok: true,
                            error: None,
                            pull: None,
                            status: (case != "missing-status").then_some(StatusPayload {
                                daemon: true,
                                paused: false,
                                connected: true,
                                peer_online: false,
                                room_id: None,
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
                }
            });
            app.refresh();
            app.rt.block_on(worker).unwrap();
            if case == "broken-config" {
                assert_eq!(app.status.device_id, "cached-device");
                assert_eq!(app.status.relay_url, "cached-relay");
                assert_eq!(app.status.room_id.as_deref(), Some("cached-room"));
            } else {
                assert_eq!(app.status.device_id, "desktop-test-device");
                assert!(app.status.room_id.is_none());
            }
            assert_eq!(
                app.status.daemon,
                matches!(case, "missing-room" | "broken-config")
            );
            assert_eq!(app.status.connected, app.status.daemon);
        }

        let (_root, mut app) = fixture();
        let ui = HeadlessUi::default();
        ui.text(&mut app);
        app.status.daemon = true;
        app.status.connected = true;
        ui.paint(&mut app, 1.0, Vec::new());
        assert!(!app.status.daemon && !app.status.connected);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn headless_background_sync_control_reports_isolated_service_results() {
        const CHILD: &str = "CLIPSYNC_TEST_DESKTOP_SERVICE_CHILD";
        let Ok(case) = std::env::var(CHILD) else {
            use std::os::unix::fs::PermissionsExt;
            let root = tempfile::tempdir().unwrap();
            let tools = root.path().join("tools");
            std::fs::create_dir(&tools).unwrap();
            for name in ["launchctl", "systemctl"] {
                let path = tools.join(name);
                std::fs::write(
                    &path,
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$CLIPSYNC_TEST_SERVICE_LOG\"\nexit 0\n",
                )
                .unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            for case in ["success", "failure"] {
                let home = root.path().join(case);
                if case == "success" {
                    std::fs::create_dir(&home).unwrap();
                } else {
                    std::fs::write(&home, "a file cannot contain login-service directories")
                        .unwrap();
                }
                let log = root.path().join(format!("{case}.log"));
                let output = child_output(std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "tests::headless_background_sync_control_reports_isolated_service_results", "--nocapture"])
                    .env(CHILD, case)
                    .env("HOME", &home)
                    .env("USERPROFILE", &home)
                    .env("PATH", &tools)
                    .env("CLIPSYNC_TEST_SERVICE_TOOLS", &tools)
                    .env("CLIPSYNC_TEST_SERVICE_LOG", &log)
                    .env_remove(clipsync_daemon::LISTEN_ENV));
                assert!(
                    output.status.success(),
                    "{} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if case == "success" {
                    let calls = std::fs::read_to_string(log).unwrap();
                    #[cfg(target_os = "macos")]
                    {
                        assert!(calls.contains("bootstrap") && calls.contains("kickstart"));
                        assert!(home
                            .join("Library/LaunchAgents/dev.clipsync.daemon.plist")
                            .is_file());
                    }
                    #[cfg(target_os = "linux")]
                    {
                        assert!(calls.contains("daemon-reload") && calls.contains("enable --now"));
                        assert!(home.join(".config/systemd/user/clipsync.service").is_file());
                        assert!(home.join(".config/autostart/clipsync.desktop").is_file());
                    }
                } else {
                    assert!(
                        !log.exists(),
                        "installation must fail before calling a service manager"
                    );
                }
            }
            return;
        };
        // Every executable search is confined to successful fake managers. This
        // prevents service installation from reaching the user's login session
        // or falling back to spawning a real daemon from the test executable.
        assert_eq!(
            std::env::var_os("PATH"),
            std::env::var_os("CLIPSYNC_TEST_SERVICE_TOOLS")
        );
        let (_root, mut app) = fixture();
        app.status.room_id = Some("paired-room".into());
        let ui = HeadlessUi::default();
        ui.click(&mut app, "Start background sync");
        if case == "failure" {
            let error = app
                .error
                .clone()
                .expect("failed install must reach the window");
            assert!(ui.text(&mut app).contains(&error));
        } else {
            assert!(app.error.is_none());
            assert!(ui.text(&mut app).contains("Paired"));
        }
    }

    #[test]
    fn constructor_respects_selected_home_and_reports_configuration_errors() {
        let (root, _app) = fixture();
        for case in ["valid", "default-relay", "invalid", "startup-invalid"] {
            if matches!(case, "invalid" | "startup-invalid") {
                std::fs::write(root.path().join("config/config.toml"), "broken = [").unwrap();
            }
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "tests::constructor_child", "--nocapture"])
                .env("CLIPSYNC_TEST_DESKTOP_CASE", case)
                .env("CLIPSYNC_HOME", root.path())
                .env("CLIPSYNC_RELAY_URL", "http://127.0.0.1:19")
                .env_remove("CLIPSYNC_LISTEN");
            if case == "default-relay" {
                command.env_remove("CLIPSYNC_RELAY_URL");
            }
            let output = child_output(&mut command);
            if case == "startup-invalid" {
                assert_eq!(output.status.code(), Some(1));
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(stderr.contains("clipsync-desktop:") && stderr.contains("toml"));
                continue;
            }
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
        if case == "startup-invalid" {
            // Invalid configuration exits before native window initialization.
            let _ = main();
            panic!("startup must reject malformed configuration");
        } else if case == "invalid" {
            assert!(DesktopApp::new().err().unwrap().contains("toml"));
        } else {
            let app = DesktopApp::new().unwrap();
            assert_eq!(app.status.device_id, "desktop-test-device");
            assert_eq!(
                app.status.relay_url,
                if case == "default-relay" {
                    "https://clipsync.develicit.dev"
                } else {
                    "http://127.0.0.1:19"
                }
            );
            assert!(app.status.room_id.is_none());
        }
    }
}
