//! Host-OS clipboard contract. CI runs this on Linux (Xvfb + xclip), macOS, and Windows.

use clipsync_clipboard::{ClipboardItem, SystemClipboard};

fn expect_adapter(name: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        return matches!(name, "macos-nspasteboard" | "macos-pbpaste");
    }
    #[cfg(target_os = "linux")]
    {
        return matches!(
            name,
            "wayland-wl-clipboard" | "x11-xclip" | "x11-xsel" | "headless"
        );
    }
    #[cfg(target_os = "windows")]
    {
        return matches!(name, "windows-clipboard" | "arboard");
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = name;
        true
    }
}

#[tokio::test]
async fn platform_adapter_matches_host() {
    let clip = SystemClipboard::open().expect("clipboard adapter");
    let info = clip.info();
    assert!(
        expect_adapter(info.name),
        "unexpected adapter {} on this OS (source {})",
        info.name,
        info.source
    );
    assert!(
        info.capabilities.text,
        "text sync is required on every host"
    );
}

#[tokio::test]
async fn platform_text_roundtrip() {
    let clip = SystemClipboard::open().expect("clipboard adapter");
    if clip.info().name == "headless" {
        assert!(
            std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none(),
            "graphical session is up but no clipboard tool was selected"
        );
        return;
    }
    let token = format!("clipsync-platform-{}", std::process::id());
    clip.write(&ClipboardItem::Text {
        text: token.clone(),
    })
    .await
    .expect("write text");
    let got = clip.read().await.expect("read text");
    match got {
        Some(ClipboardItem::Text { text }) => {
            assert!(
                text.contains(&token),
                "clipboard returned {text:?}, expected {token}"
            );
        }
        other => panic!("expected text item, got {other:?}"),
    }
}
