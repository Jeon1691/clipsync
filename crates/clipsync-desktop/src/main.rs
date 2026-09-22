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

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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

        egui::CentralPanel::default().show(ctx, |ui| {
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

fn main() -> eframe::Result {
    let app = match DesktopApp::new() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("clipsync-desktop: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ClipSync")
            .with_inner_size([440.0, 560.0])
            .with_min_inner_size([380.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("ClipSync", options, Box::new(|_cc| Ok(Box::new(app))))
}
