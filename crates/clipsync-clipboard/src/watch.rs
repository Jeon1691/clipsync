// Copyright the ClipSync contributors.
// SPDX-License-Identifier: MIT
use std::time::Duration;

use clipsync_protocol::{DEBOUNCE_MS, WATCH_POLL_MS};

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
            tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;
            let sig = clipboard.signature().await?;
            let item = clipboard.read().await?;
            return Ok((sig, item));
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::{AdapterInfo, Capabilities, ClipboardBackend, ClipboardError};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    struct Sequence {
        signatures: Mutex<VecDeque<std::result::Result<&'static str, &'static str>>>,
        item: Option<ClipboardItem>,
        read_error: bool,
    }
    #[async_trait]
    impl ClipboardBackend for Sequence {
        fn name(&self) -> &'static str {
            "scripted-provider"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::all()
        }
        async fn read(&self) -> Result<Option<ClipboardItem>> {
            if self.read_error {
                Err(ClipboardError::Unavailable("read unavailable".into()))
            } else {
                Ok(self.item.clone())
            }
        }
        async fn write(&self, _: &ClipboardItem) -> Result<()> {
            unreachable!("watcher must not write")
        }
        async fn signature(&self) -> Result<String> {
            self.signatures
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected extra poll")
                .map(str::to_string)
                .map_err(|e| ClipboardError::Unavailable(e.into()))
        }
    }
    fn clipboard(
        signatures: &[std::result::Result<&'static str, &'static str>],
        item: Option<ClipboardItem>,
        read_error: bool,
    ) -> SystemClipboard {
        SystemClipboard::from_backend(
            Box::new(Sequence {
                signatures: Mutex::new(signatures.iter().copied().collect()),
                item,
                read_error,
            }),
            AdapterInfo {
                name: "scripted-provider",
                source: "injected",
                capabilities: Capabilities::all(),
                fallback_reason: None,
            },
        )
    }
    #[tokio::test]
    async fn watcher_skips_unchanged_signature_and_returns_the_debounced_snapshot() {
        let item = ClipboardItem::Text {
            text: "settled content".into(),
        };
        let clipboard = clipboard(
            &[Ok("old"), Ok("transient"), Ok("settled")],
            Some(item.clone()),
            false,
        );
        let (signature, result) =
            tokio::time::timeout(Duration::from_secs(3), wait_for_change(&clipboard, "old"))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(signature, "settled");
        assert_eq!(result, Some(item));
    }
    #[tokio::test]
    async fn watcher_can_report_a_clipboard_becoming_empty() {
        let clipboard = clipboard(&[Ok("cleared"), Ok("cleared")], None, false);
        let (signature, result) = wait_for_change(&clipboard, "old").await.unwrap();
        assert_eq!(signature, "cleared");
        assert!(result.is_none());
    }
    #[tokio::test]
    async fn watcher_returns_provider_failures_at_each_read_boundary() {
        for signatures in [
            vec![Err("first signature")],
            vec![Ok("changed"), Err("debounced signature")],
        ] {
            let clipboard = clipboard(&signatures, None, false);
            let error = wait_for_change(&clipboard, "old").await.unwrap_err();
            assert!(matches!(error, ClipboardError::Unavailable(_)));
        }
        let clipboard = clipboard(&[Ok("changed"), Ok("settled")], None, true);
        assert!(wait_for_change(&clipboard, "old")
            .await
            .unwrap_err()
            .to_string()
            .contains("read unavailable"));
    }
}
