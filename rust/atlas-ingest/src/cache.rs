//! The text cache `read_article` fills and `save_article` spends.
//!
//! Port of `ingest._READ_CACHE`, `cache_put`, `cache_get` and `_cache_prune`.
//!
//! The reason it exists is a hard limit, not an optimisation: the local model
//! runs with a 6 144-token context, so a 9 000-character article returned as a
//! tool result overflows it and llama.cpp answers HTTP 400 after two reads. The
//! model therefore gets a ~1 200-character preview, and the full text waits here
//! until `save_article` asks for it — including across a restart, because the
//! files survive.
//!
//! Properties that are rules, not preferences:
//!
//! * keyed by the **normalised** URL (`atlas-text`'s `norm_url`), so the same
//!   story found twice with different tracking parameters is one entry;
//! * a text shorter than [`crate::MIN_CHARS`] is never stored — a teaser is not a
//!   body, and `read_article` refuses it before it gets here anyway;
//! * the newest [`crate::READ_CACHE_MAX`] pages are in memory, the newest
//!   [`crate::CACHE_KEEP`] files are on disk; both caps are enforced on write, so
//!   the directory cannot grow without bound;
//! * a read-only or unwritable data directory never breaks reading: the write is
//!   counted in [`CacheStats::write_errors`] and the page is still usable from
//!   memory.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::hash::sha1_hex;

/// One cached page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The URL as it was read (not the key).
    pub url: String,
    /// The headline the caller passed, when it had one.
    pub title: String,
    pub text: String,
    /// `db.now_iso()` when it was cached.
    pub at: String,
}

/// What the cache did — counts only, never text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub puts: usize,
    pub gets: usize,
    pub hits: usize,
    pub misses: usize,
    /// Entries dropped from memory because the memory cap was reached.
    pub evicted: usize,
    /// Files removed from disk by the prune.
    pub pruned: usize,
    /// Files that could not be written (read-only data dir, full disk).
    pub write_errors: usize,
    pub promoted: usize,
}

/// The in-memory LRU plus its directory.
#[derive(Debug)]
pub struct TextCache {
    dir: PathBuf,
    memory_cap: usize,
    file_cap: usize,
    order: VecDeque<String>,
    items: HashMap<String, Entry>,
    stats: CacheStats,
}

impl TextCache {
    pub fn new(dir: PathBuf, memory_cap: usize, file_cap: usize) -> TextCache {
        TextCache {
            dir,
            memory_cap: memory_cap.max(1),
            file_cap: file_cap.max(1),
            order: VecDeque::new(),
            items: HashMap::new(),
            stats: CacheStats::default(),
        }
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where one key's text lives on disk — `_cache_path(key)`.
    pub fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.txt", sha1_hex(key.as_bytes())))
    }

    /// `cache_put` — remember the full text of one URL. Never fails a caller.
    ///
    /// Returns whether the text was kept: a body shorter than
    /// [`crate::MIN_CHARS`] is not article text and is refused here as well as in
    /// `read_article`.
    pub fn put(&mut self, key: &str, url: &str, title: &str, text: &str, now: &str) -> bool {
        let key = key.trim();
        let text = text.trim();
        if key.is_empty() || text.chars().count() < crate::MIN_CHARS {
            return false;
        }
        let capped: String = text.chars().take(crate::MAX_CACHE_CHARS).collect();
        self.stats.puts += 1;
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.to_string());
        self.items.insert(
            key.to_string(),
            Entry {
                url: url.to_string(),
                title: title.to_string(),
                text: capped.clone(),
                at: now.to_string(),
            },
        );
        while self.order.len() > self.memory_cap {
            if let Some(oldest) = self.order.pop_front() {
                if self.items.remove(&oldest).is_some() {
                    self.stats.evicted += 1;
                }
            }
        }
        self.write_file(key, &capped);
        true
    }

    /// `cache_get` — the full text for a URL key, memory first, then disk.
    ///
    /// A disk hit is promoted to memory, exactly like the Python cache, so a
    /// `save_article` straight after a restart costs one read.
    pub fn get(&mut self, key: &str) -> Option<String> {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        self.stats.gets += 1;
        if let Some(entry) = self.items.get(key) {
            let text = entry.text.clone();
            self.stats.hits += 1;
            // mark as most recent; the entry itself stays where it is
            self.touch(key);
            return Some(text);
        }
        let path = self.path_for(key);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => {
                self.stats.misses += 1;
                return None;
            }
        };
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if text.chars().count() < crate::MIN_CHARS {
            // a half-written or truncated file is not a body
            let _ = std::fs::remove_file(&path);
            self.stats.misses += 1;
            return None;
        }
        self.stats.hits += 1;
        self.stats.promoted += 1;
        let capped: String = text.chars().take(crate::MAX_CACHE_CHARS).collect();
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.to_string());
        self.items.insert(
            key.to_string(),
            Entry {
                url: String::new(),
                title: String::new(),
                text: capped.clone(),
                at: String::new(),
            },
        );
        while self.order.len() > self.memory_cap {
            if let Some(oldest) = self.order.pop_front() {
                self.items.remove(&oldest);
                self.stats.evicted += 1;
            }
        }
        Some(capped)
    }

    /// Is this key in memory (a cheap check, no disk access)?
    pub fn in_memory(&self, key: &str) -> bool {
        self.items.contains_key(key)
    }

    /// Forget one key, in memory and on disk.
    pub fn forget(&mut self, key: &str) -> bool {
        self.order.retain(|existing| existing != key);
        let removed = self.items.remove(key).is_some();
        let path = self.path_for(key);
        let _ = std::fs::remove_file(&path);
        removed
    }

    fn touch(&mut self, key: &str) {
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.to_string());
    }

    fn write_file(&mut self, key: &str, text: &str) {
        if std::fs::create_dir_all(&self.dir).is_err() {
            self.stats.write_errors += 1;
            return;
        }
        if std::fs::write(self.path_for(key), text.as_bytes()).is_err() {
            self.stats.write_errors += 1;
            return;
        }
        self.prune();
    }

    /// `_cache_prune` — keep the newest `file_cap` files, remove the rest.
    fn prune(&mut self) {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|ext| ext != "txt").unwrap_or(true) {
                continue;
            }
            let stamp = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            files.push((stamp, path));
        }
        if files.len() <= self.file_cap {
            return;
        }
        files.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, path) in files.split_off(self.file_cap) {
            if std::fs::remove_file(&path).is_ok() {
                self.stats.pruned += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(marker: &str) -> String {
        let mut text = format!("Sentence about {}. ", marker);
        while text.chars().count() < crate::MIN_CHARS + 50 {
            text.push_str("The ministry said the measure would take effect. ");
        }
        text
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "atlas-ingest-cache-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|delta| delta.subsec_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_miss_then_a_hit() {
        let dir = temp_dir("hit");
        let mut cache = TextCache::new(dir.clone(), 4, 10);
        assert_eq!(cache.get("https://example.com/a"), None);
        assert!(cache.put("https://example.com/a", "https://example.com/a", "T", &body("a"), "2026-11-05T10:00:00Z"));
        assert!(cache.get("https://example.com/a").is_some());
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_teaser_is_never_cached() {
        let dir = temp_dir("teaser");
        let mut cache = TextCache::new(dir.clone(), 4, 10);
        assert!(!cache.put("k", "u", "T", "only a teaser, far too short", "now"));
        assert_eq!(cache.len(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_memory_cap_evicts_the_oldest() {
        let dir = temp_dir("evict");
        let mut cache = TextCache::new(dir.clone(), 2, 100);
        for index in 0..5 {
            let key = format!("https://example.com/{}", index);
            cache.put(&key, &key, "", &body(&index.to_string()), "now");
        }
        assert_eq!(cache.len(), 2);
        assert!(!cache.in_memory("https://example.com/0"));
        assert!(cache.in_memory("https://example.com/4"));
        assert_eq!(cache.stats().evicted, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_cap_prunes_the_oldest_files() {
        let dir = temp_dir("prune");
        let mut cache = TextCache::new(dir.clone(), 100, 3);
        for index in 0..6 {
            let key = format!("https://example.com/p{}", index);
            cache.put(&key, &key, "", &body(&index.to_string()), "now");
            // the prune sorts on mtime; make each file older than the next
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        let files = std::fs::read_dir(&dir).map(|entries| entries.count()).unwrap_or(0);
        assert!(files <= 3, "{} files kept, cap is 3", files);
        assert!(cache.stats().pruned >= 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_disk_entry_survives_a_restart_and_is_promoted() {
        let dir = temp_dir("restart");
        let key = "https://example.com/restart";
        {
            let mut cache = TextCache::new(dir.clone(), 4, 10);
            cache.put(key, key, "T", &body("restart"), "now");
        }
        let mut fresh = TextCache::new(dir.clone(), 4, 10);
        assert!(!fresh.in_memory(key));
        let text = fresh.get(key).expect("the file must still be there");
        assert!(text.contains("Sentence about restart"));
        assert!(fresh.in_memory(key), "a disk hit is promoted to memory");
        assert_eq!(fresh.stats().promoted, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_capped_key_and_a_capped_body() {
        let dir = temp_dir("cap");
        let mut cache = TextCache::new(dir.clone(), 4, 10);
        let long = "word ".repeat(crate::MAX_CACHE_CHARS);
        assert!(cache.put("k", "u", "", &long, "now"));
        let kept = cache.get("k").unwrap();
        assert_eq!(kept.chars().count(), crate::MAX_CACHE_CHARS);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unwritable_directory_counts_the_error_instead_of_failing() {
        let mut cache = TextCache::new(PathBuf::from("/dev/null/nope"), 4, 10);
        assert!(cache.put("k", "u", "", &body("k"), "now"));
        assert_eq!(cache.stats().write_errors, 1);
        assert!(cache.get("k").is_some(), "memory still answers");
    }
}
