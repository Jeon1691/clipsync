use std::collections::VecDeque;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use clipsync_clipboard::ClipboardItem;
use clipsync_protocol::{LOOP_CACHE_TTL_SECS, REPLAY_CACHE_TTL_SECS};

pub struct LoopGuard {
    hashes: VecDeque<(Instant, [u8; 32])>,
    message_ids: VecDeque<(Instant, String)>,
}

impl Default for LoopGuard {
    fn default() -> Self {
        Self {
            hashes: VecDeque::new(),
            message_ids: VecDeque::new(),
        }
    }
}

impl LoopGuard {
    pub fn remember_remote(&mut self, message_id: &str, item: &ClipboardItem) {
        self.gc();
        self.hashes.push_back((Instant::now(), content_hash(item)));
        self.message_ids
            .push_back((Instant::now(), message_id.to_string()));
    }

    #[allow(dead_code)]
    pub fn remember_hash(&mut self, hash: [u8; 32]) {
        self.gc();
        self.hashes.push_back((Instant::now(), hash));
    }

    pub fn is_echo(&self, item: &ClipboardItem) -> bool {
        let h = content_hash(item);
        self.hashes.iter().any(|(_, x)| x == &h)
    }

    pub fn seen_message(&self, id: &str) -> bool {
        self.message_ids.iter().any(|(_, x)| x == id)
    }

    fn gc(&mut self) {
        self.gc_at(Instant::now());
    }

    fn gc_at(&mut self, now: Instant) {
        let hash_ttl = Duration::from_secs(LOOP_CACHE_TTL_SECS);
        let id_ttl = Duration::from_secs(REPLAY_CACHE_TTL_SECS);
        while self
            .hashes
            .front()
            .map(|f| now.saturating_duration_since(f.0) > hash_ttl)
            .unwrap_or(false)
        {
            self.hashes.pop_front();
        }
        while self
            .message_ids
            .front()
            .map(|f| now.saturating_duration_since(f.0) > id_ttl)
            .unwrap_or(false)
        {
            self.message_ids.pop_front();
        }
    }
}

pub fn content_hash(item: &ClipboardItem) -> [u8; 32] {
    let mut hasher = Sha256::new();
    match item {
        ClipboardItem::Text { text } => {
            hasher.update(b"text:");
            hasher.update(text.as_bytes());
        }
        ClipboardItem::Image { bytes, mime, .. } => {
            hasher.update(b"image:");
            hasher.update(mime.as_str().as_bytes());
            hasher.update(bytes);
        }
        ClipboardItem::Files { files } => {
            hasher.update(b"files:");
            for f in files {
                hasher.update(f.name.as_bytes());
                hasher.update(&(f.bytes.len() as u64).to_be_bytes());
                hasher.update(&f.bytes);
            }
        }
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_echo_cache_expires() {
        let mut g = LoopGuard::default();
        let item = ClipboardItem::Text { text: "x".into() };
        g.remember_remote("m", &item);
        let later = Instant::now() + Duration::from_secs(REPLAY_CACHE_TTL_SECS + 2);
        g.gc_at(later);
        assert!(!g.is_echo(&item));
        assert!(!g.seen_message("m"));
    }

    #[test]
    fn echo_detected() {
        let mut g = LoopGuard::default();
        let item = ClipboardItem::Text {
            text: "hello".into(),
        };
        g.remember_remote("m1", &item);
        assert!(g.is_echo(&item));
        assert!(!g.is_echo(&ClipboardItem::Text {
            text: "other".into()
        }));
    }
}
