//! Files placed here exist so Finder (and other desktops) can paste an image.
//! The pasteboard still carries PNG/TIFF for apps that want pixels. A later
//! read treats paths inside this directory as that image, so the original
//! bytes stay intact and the sync loop does not echo them as a new file copy.

use std::path::{Path, PathBuf};

use crate::item::ImageMime;
use crate::Result;

pub fn pasteboard_dir() -> PathBuf {
    if let Ok(root) = std::env::var("CLIPSYNC_HOME") {
        if !root.is_empty() {
            return PathBuf::from(root).join("data").join("pasteboard");
        }
    }
    if let Ok(data) = std::env::var("CLIPSYNC_DATA_DIR") {
        if !data.is_empty() {
            return PathBuf::from(data).join("pasteboard");
        }
    }
    directories::ProjectDirs::from("dev", "clipsync", "clipsync")
        .map(|dirs| dirs.data_dir().join("pasteboard"))
        .unwrap_or_else(|| std::env::temp_dir().join("clipsync-pasteboard"))
}

pub fn stage_image(mime: ImageMime, bytes: &[u8]) -> Result<PathBuf> {
    stage_image_in(&pasteboard_dir(), mime, bytes)
}

pub fn stage_image_in(dir: &Path, mime: ImageMime, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir);
    let name = format!("clipboard.{}", extension(mime));
    let path = dir.join(name);
    let partial = dir.join(format!(
        ".{}.partial",
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("clipboard")
    ));
    std::fs::write(&partial, bytes)?;
    restrict_file(&partial);
    std::fs::rename(&partial, &path)?;
    restrict_file(&path);
    Ok(path)
}

pub fn sweep_other_clipboard_files(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with("clipboard.") || name.starts_with(".clipboard.") {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn paths_are_image_sidecars(paths: &[PathBuf]) -> bool {
    match paths {
        [path] => is_image_sidecar(path),
        _ => false,
    }
}

fn is_image_sidecar(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    if !matches!(name, "clipboard.png" | "clipboard.jpg" | "clipboard.webp") {
        return false;
    }
    path_is_inside(&pasteboard_dir(), path)
}

fn path_is_inside(root: &Path, path: &Path) -> bool {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(&root)
}

fn extension(mime: ImageMime) -> &'static str {
    match mime {
        ImageMime::Png => "png",
        ImageMime::Jpeg => "jpg",
        ImageMime::Webp => "webp",
    }
}

fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
pub(crate) fn with_stage_root<T>(root: &Path, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|err| err.into_inner());
    let prev_home = std::env::var("CLIPSYNC_HOME").ok();
    set_env("CLIPSYNC_HOME", Some(&root.to_string_lossy()));
    let out = f();
    set_env("CLIPSYNC_HOME", prev_home.as_deref());
    out
}

#[cfg(test)]
fn set_env(key: &str, value: Option<&str>) {
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_original_image_bytes_under_clipboard_name() {
        let root = std::env::temp_dir().join(format!("clipsync-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bytes = b"\x89PNG\r\n\x1a\nrest";
        let path = with_stage_root(&root, || {
            let path = stage_image(ImageMime::Png, bytes).unwrap();
            assert!(paths_are_image_sidecars(std::slice::from_ref(&path)));
            path
        });
        assert_eq!(
            path,
            root.join("data").join("pasteboard").join("clipboard.png")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sidecar_check_rejects_paths_outside_the_stage_dir() {
        let root = std::env::temp_dir().join(format!("clipsync-stage-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let outside = root.join("notes.txt");
        std::fs::write(&outside, b"hi").unwrap();
        with_stage_root(&root, || {
            assert!(!paths_are_image_sidecars(std::slice::from_ref(&outside)));
        });
        let _ = std::fs::remove_dir_all(&root);
    }
}
