//! Desktop pairing and daemon control shared by the window.

use clipsync_core::{begin_create_room, join_room, App};
use clipsync_daemon::install_and_start;
use clipsync_storage::LocalStore;

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

pub async fn create_room(store: LocalStore) -> Result<String, String> {
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
        if pending.wait(&store, &identity).await.is_ok() {
            let _ = install_and_start(&store.paths);
        }
    });
    Ok(code)
}

pub async fn join_room_code(app: &App, code: &str) -> Result<String, String> {
    let code = normalize_join_code(code).ok_or_else(|| "enter a 6-digit code".to_string())?;
    let identity = app.identity().map_err(|e| e.to_string())?;
    let cfg = app.store.load_config().map_err(|e| e.to_string())?;
    join_room(&app.store, &identity, &cfg, &code)
        .await
        .map_err(|e| e.to_string())?;
    let room = app.store.current_room().map_err(|e| e.to_string())?;
    install_and_start(&app.store.paths).map_err(|e| e.to_string())?;
    Ok(room.room_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_code_keeps_six_digits() {
        assert_eq!(normalize_join_code("48 29-13").as_deref(), Some("482913"));
        assert!(normalize_join_code("12345").is_none());
        assert!(normalize_join_code("1234567").is_none());
    }
}
