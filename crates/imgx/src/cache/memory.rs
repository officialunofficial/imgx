//! In-memory LRU cache. Ported from src/cache/memory.zig. Fixed maximum
//! size in bytes; when a `put` would exceed the limit, least-recently-used
//! entries are evicted until there is room. Thread-safe via a
//! `parking_lot::RwLock` (held only across pure in-memory operations,
//! never across an `.await`, so a sync lock is the right/faster choice
//! even inside async methods).

use lru::LruCache;
use parking_lot::RwLock;

use super::{Cache, CacheEntry};

struct StoredEntry {
    data: Vec<u8>,
    content_type: String,
    created_at: i64,
}

impl StoredEntry {
    fn size(&self) -> usize {
        self.data.len() + self.content_type.len()
    }
}

struct Inner {
    map: LruCache<String, StoredEntry>,
    current_size_bytes: usize,
}

/// In-memory cache holding at most `max_size_bytes` bytes of payload data
/// (keys and content-type strings count toward the total too).
pub struct MemoryCache {
    max_size_bytes: usize,
    inner: RwLock<Inner>,
}

impl MemoryCache {
    pub fn new(max_size_bytes: usize) -> Self {
        Self {
            max_size_bytes,
            inner: RwLock::new(Inner {
                map: LruCache::unbounded(),
                current_size_bytes: 0,
            }),
        }
    }
}

impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        let mut inner = self.inner.write();
        let stored = inner.map.get(key)?;
        Some(CacheEntry {
            data: stored.data.clone(),
            content_type: stored.content_type.clone(),
            created_at: stored.created_at,
        })
    }

    async fn put(&self, key: &str, entry: CacheEntry) {
        let new_size = entry.data.len() + entry.content_type.len();
        let mut inner = self.inner.write();

        // If the key already exists, remove it first (we'll replace it).
        if let Some(old) = inner.map.pop(key) {
            inner.current_size_bytes -= old.size();
        }

        // Evict LRU entries until there is room (or the map is empty).
        while inner.current_size_bytes + new_size > self.max_size_bytes && !inner.map.is_empty() {
            if let Some((_, evicted)) = inner.map.pop_lru() {
                inner.current_size_bytes -= evicted.size();
            }
        }

        // If the single entry is bigger than the whole cache, skip storing.
        if new_size > self.max_size_bytes {
            return;
        }

        inner.current_size_bytes += new_size;
        inner.map.put(
            key.to_string(),
            StoredEntry {
                data: entry.data,
                content_type: entry.content_type,
                created_at: entry.created_at,
            },
        );
    }

    async fn delete(&self, key: &str) -> bool {
        let mut inner = self.inner.write();
        match inner.map.pop(key) {
            Some(old) => {
                inner.current_size_bytes -= old.size();
                true
            }
            None => false,
        }
    }

    async fn clear(&self) {
        let mut inner = self.inner.write();
        inner.map.clear();
        inner.current_size_bytes = 0;
    }

    async fn size(&self) -> usize {
        self.inner.read().map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(data: &str, content_type: &str, created_at: i64) -> CacheEntry {
        CacheEntry {
            data: data.as_bytes().to_vec(),
            content_type: content_type.to_string(),
            created_at,
        }
    }

    #[tokio::test]
    async fn memory_cache_put_and_get_a_value() {
        let mc = MemoryCache::new(4096);
        mc.put("photo1", entry("image-bytes", "image/png", 1_700_000_000))
            .await;

        let got = mc.get("photo1").await.unwrap();
        assert_eq!(got.data, b"image-bytes");
        assert_eq!(got.content_type, "image/png");
        assert_eq!(got.created_at, 1_700_000_000);
    }

    #[tokio::test]
    async fn memory_cache_get_missing_key_returns_none() {
        let mc = MemoryCache::new(4096);
        assert!(mc.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn memory_cache_delete_existing_key_returns_true() {
        let mc = MemoryCache::new(4096);
        mc.put("k", entry("v", "t", 0)).await;
        assert!(mc.delete("k").await);
        assert!(mc.get("k").await.is_none());
    }

    #[tokio::test]
    async fn memory_cache_delete_missing_key_returns_false() {
        let mc = MemoryCache::new(4096);
        assert!(!mc.delete("nope").await);
    }

    #[tokio::test]
    async fn memory_cache_size_tracks_correctly() {
        let mc = MemoryCache::new(4096);
        assert_eq!(mc.size().await, 0);

        mc.put("a", entry("1", "t", 0)).await;
        assert_eq!(mc.size().await, 1);

        mc.put("b", entry("2", "t", 0)).await;
        assert_eq!(mc.size().await, 2);

        mc.delete("a").await;
        assert_eq!(mc.size().await, 1);
    }

    #[tokio::test]
    async fn memory_cache_eviction_when_max_size_exceeded() {
        // Max size = 20 bytes. Each entry has data.len + content_type.len
        // counted toward the total.
        let mc = MemoryCache::new(20);

        // Entry A: 10 bytes data + 1 byte ct = 11 bytes (total 11)
        mc.put("a", entry("0123456789", "t", 1)).await;
        assert_eq!(mc.size().await, 1);

        // Entry B: 10 bytes data + 1 byte ct = 11 bytes (total 22 > 20)
        // This should evict entry A to make room.
        mc.put("b", entry("abcdefghij", "t", 2)).await;
        assert_eq!(mc.size().await, 1);
        assert!(mc.get("a").await.is_none()); // evicted
        assert!(mc.get("b").await.is_some()); // still present
    }

    #[tokio::test]
    async fn memory_cache_eviction_evicts_least_recently_used() {
        // Max size = 30 bytes.
        let mc = MemoryCache::new(30);

        mc.put("a", entry("aaaaa", "t", 1)).await; // 6 bytes (total 6)
        mc.put("b", entry("bbbbb", "t", 2)).await; // 6 bytes (total 12)
        mc.put("c", entry("ccccc", "t", 3)).await; // 6 bytes (total 18)

        // Access A so it becomes recently used (B is now LRU).
        mc.get("a").await;

        // Entry D: 15+1=16 bytes. Need to free at least 16+18-30=4 bytes.
        // LRU is B (6 bytes freed -> total becomes 12+16=28, fits).
        mc.put("d", entry("ddddddddddddddd", "t", 4)).await;

        assert!(mc.get("b").await.is_none()); // evicted (was LRU)
        assert!(mc.get("a").await.is_some()); // kept (was accessed)
        assert!(mc.get("d").await.is_some()); // newly added
    }

    #[tokio::test]
    async fn memory_cache_clear_empties_cache() {
        let mc = MemoryCache::new(4096);
        mc.put("x", entry("data", "ct", 0)).await;
        mc.put("y", entry("data", "ct", 0)).await;
        assert_eq!(mc.size().await, 2);

        mc.clear().await;
        assert_eq!(mc.size().await, 0);
        assert!(mc.get("x").await.is_none());
        assert!(mc.get("y").await.is_none());
    }

    #[tokio::test]
    async fn memory_cache_overwrite_existing_key() {
        let mc = MemoryCache::new(4096);
        mc.put("key", entry("old", "t", 1)).await;
        mc.put("key", entry("new", "t", 2)).await;

        assert_eq!(mc.size().await, 1);
        let got = mc.get("key").await.unwrap();
        assert_eq!(got.data, b"new");
        assert_eq!(got.created_at, 2);
    }
}
