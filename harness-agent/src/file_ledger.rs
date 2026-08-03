use std::collections::{HashMap, VecDeque};

const DEFAULT_MAX_ENTRIES: usize = 100;
const DEFAULT_MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    pub content_hash: u64,
    pub mtime_ms: u64,
    pub full_read: bool,
}

pub struct FileLedger {
    entries: HashMap<String, LedgerEntry>,
    order: VecDeque<String>,
    byte_sizes: HashMap<String, usize>,
    total_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for FileLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl FileLedger {
    pub fn new() -> Self {
        Self::with_caps(DEFAULT_MAX_ENTRIES, DEFAULT_MAX_BYTES)
    }

    pub fn with_caps(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            byte_sizes: HashMap::new(),
            total_bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    pub fn record(&mut self, key: &str, content: &str, mtime_ms: u64, full_read: bool) {
        let key = key.to_string();
        if let Some(old_size) = self.byte_sizes.remove(&key) {
            self.total_bytes = self.total_bytes.saturating_sub(old_size);
        }
        self.order.retain(|existing| existing != &key);

        let byte_len = content.len();
        self.total_bytes = self.total_bytes.saturating_add(byte_len);
        self.byte_sizes.insert(key.clone(), byte_len);
        self.entries.insert(
            key.clone(),
            LedgerEntry {
                content_hash: fnv1a(content.as_bytes()),
                mtime_ms,
                full_read,
            },
        );
        self.order.push_front(key);
        self.evict_over_caps();
    }

    pub fn get(&self, key: &str) -> Option<&LedgerEntry> {
        self.entries.get(key)
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    fn evict_over_caps(&mut self) {
        while self.entries.len() > self.max_entries || self.total_bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_back() else {
                break;
            };
            self.entries.remove(&oldest);
            if let Some(byte_len) = self.byte_sizes.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(byte_len);
            }
        }
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_get_roundtrip() {
        let mut l = FileLedger::new();
        l.record("/w/a.rs", "fn a(){}", 100, true);
        let e = l.get("/w/a.rs").unwrap();
        assert_eq!(e.mtime_ms, 100);
        assert!(e.full_read);
        assert_eq!(e.content_hash, fnv1a(b"fn a(){}"));
    }
    #[test]
    fn lru_evicts_oldest_over_entry_cap() {
        let mut l = FileLedger::with_caps(2, 10_000);
        l.record("/w/a", "x", 1, true);
        l.record("/w/b", "x", 1, true);
        l.record("/w/c", "x", 1, true); // 超 2 条 → 淘汰最早的 a
        assert!(l.get("/w/a").is_none());
        assert!(l.get("/w/b").is_some());
        assert!(l.get("/w/c").is_some());
        assert_eq!(l.len(), 2);
    }
    #[test]
    fn lru_evicts_over_byte_cap() {
        let mut l = FileLedger::with_caps(100, 8);
        l.record("/w/a", "aaaa", 1, true); // 4 bytes
        l.record("/w/b", "bbbb", 1, true); // 4 bytes·累计 8
        l.record("/w/c", "cccc", 1, true); // 超 8 → 淘汰 a
        assert!(l.get("/w/a").is_none());
        assert!(l.get("/w/c").is_some());
    }
    #[test]
    fn re_record_updates_and_refreshes_recency() {
        let mut l = FileLedger::with_caps(2, 10_000);
        l.record("/w/a", "x", 1, true);
        l.record("/w/b", "x", 1, true);
        l.record("/w/a", "y", 2, false); // 更新 a + 移到队首
        l.record("/w/c", "x", 1, true); // 淘汰最久未记录的 b（非 a）
        assert!(l.get("/w/b").is_none());
        assert_eq!(l.get("/w/a").unwrap().mtime_ms, 2);
        assert!(!l.get("/w/a").unwrap().full_read);
    }
}
