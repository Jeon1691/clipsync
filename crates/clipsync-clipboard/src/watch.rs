use std::time::Duration;

use clipsync_protocol::WATCH_POLL_MS;

use crate::item::ClipboardItem;
use crate::Result;
use crate::SystemClipboard;

pub async fn wait_for_change(
    clipboard: &SystemClipboard,
    last_sig: &str,
) -> Result<(String, Option<ClipboardItem>)> {
    loop {
        tokio::time::sleep(Duration::from_millis(WATCH_POLL_MS)).await;
        let sig = clipboard.signature().await?;
        if sig != last_sig {
            let item = clipboard.read().await?;
            return Ok((sig, item));
        }
    }
}
