//! Tiered cache. Ported from src/cache/tiered.zig. Composes a fast L1
//! (e.g. in-memory) and a persistent L2 (e.g. R2) into a two-level
//! hierarchy. Writes go to L1 synchronously; L2 writes are dispatched via
//! `tokio::spawn` to keep upload latency off the critical path — Zig used
//! a thread pool for the same reason.
//!
//! Generic over `L1`/`L2` rather than `dyn Cache`: the backend set is
//! closed, so this avoids the object-safety/boxing cost of trait objects.

use std::sync::Arc;

use super::{Cache, CacheEntry};

pub struct TieredCache<L1, L2> {
    l1: Arc<L1>,
    l2: Arc<L2>,
}

impl<L1, L2> TieredCache<L1, L2>
where
    L1: Cache + 'static,
    L2: Cache + 'static,
{
    pub fn new(l1: L1, l2: L2) -> Self {
        Self {
            l1: Arc::new(l1),
            l2: Arc::new(l2),
        }
    }

    /// Schedule an async L2 write on the tokio runtime. Best-effort: if
    /// the task can't be spawned or the write fails, it's silently
    /// dropped, matching the Zig implementation's fire-and-forget design.
    fn put_l2_async(&self, key: &str, entry: CacheEntry) {
        let l2 = Arc::clone(&self.l2);
        let key = key.to_string();
        tokio::spawn(async move {
            l2.put(&key, entry).await;
        });
    }
}

impl<L1, L2> Cache for TieredCache<L1, L2>
where
    L1: Cache + 'static,
    L2: Cache + 'static,
{
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        // L1 hit -> return directly (fast path).
        if let Some(entry) = self.l1.get(key).await {
            return Some(entry);
        }

        // L1 miss -> check L2, promote to L1 on hit.
        if let Some(entry) = self.l2.get(key).await {
            self.l1.put(key, entry.clone()).await;
            return Some(entry);
        }

        None
    }

    async fn put(&self, key: &str, entry: CacheEntry) {
        // L1 write is synchronous (fast); L2 write is dispatched async to
        // keep the R2 upload off the response path.
        self.l1.put(key, entry.clone()).await;
        self.put_l2_async(key, entry);
    }

    async fn delete(&self, key: &str) -> bool {
        // Delete from both — no short-circuiting, both sides always run.
        let d1 = self.l1.delete(key).await;
        let d2 = self.l2.delete(key).await;
        d1 || d2
    }

    async fn clear(&self) {
        self.l1.clear().await;
        self.l2.clear().await;
    }

    async fn size(&self) -> usize {
        // L1 size only — the fast, trackable layer.
        self.l1.size().await
    }
}

#[cfg(test)]
mod tests {
    use super::super::MemoryCache;
    use super::*;

    fn entry(data: &str, content_type: &str, created_at: i64) -> CacheEntry {
        CacheEntry {
            data: data.as_bytes().to_vec(),
            content_type: content_type.to_string(),
            created_at,
        }
    }

    #[tokio::test]
    async fn tiered_get_miss_on_both_returns_none() {
        let tc = TieredCache::new(MemoryCache::new(4096), MemoryCache::new(4096));
        assert!(tc.get("nonexistent").await.is_none());
    }

    #[tokio::test]
    async fn tiered_put_stores_in_both_l1_and_l2() {
        let l1 = MemoryCache::new(4096);
        let l2 = MemoryCache::new(4096);
        let tc = TieredCache::new(l1, l2);

        tc.put("photo1", entry("image-bytes", "image/png", 1_700_000_000))
            .await;

        let l1_got = tc.l1.get("photo1").await.unwrap();
        assert_eq!(l1_got.data, b"image-bytes");
        assert_eq!(l1_got.content_type, "image/png");
        assert_eq!(l1_got.created_at, 1_700_000_000);

        // L2 write is async (tokio::spawn); yield once so it lands before
        // checking. Its body has no real suspension points (parking_lot's
        // lock is sync), so a single yield is deterministic, not timing-based.
        tokio::task::yield_now().await;

        let l2_got = tc.l2.get("photo1").await.unwrap();
        assert_eq!(l2_got.data, b"image-bytes");
        assert_eq!(l2_got.content_type, "image/png");
        assert_eq!(l2_got.created_at, 1_700_000_000);
    }

    #[tokio::test]
    async fn tiered_get_from_l1_l1_hit() {
        let l1 = MemoryCache::new(4096);
        let l2 = MemoryCache::new(4096);
        l1.put("key", entry("l1-data", "text/plain", 42)).await;
        let tc = TieredCache::new(l1, l2);

        let got = tc.get("key").await.unwrap();
        assert_eq!(got.data, b"l1-data");
        assert_eq!(got.created_at, 42);

        // L2 should still be empty (no write-back on L1 hit).
        assert!(tc.l2.get("key").await.is_none());
    }

    #[tokio::test]
    async fn tiered_get_promotes_from_l2_to_l1() {
        let l1 = MemoryCache::new(4096);
        let l2 = MemoryCache::new(4096);
        l2.put("key", entry("l2-data", "image/webp", 99)).await;
        assert!(l1.get("key").await.is_none());

        let tc = TieredCache::new(l1, l2);

        let got = tc.get("key").await.unwrap();
        assert_eq!(got.data, b"l2-data");
        assert_eq!(got.content_type, "image/webp");
        assert_eq!(got.created_at, 99);

        // L1 should now have it too (promoted).
        let l1_got = tc.l1.get("key").await.unwrap();
        assert_eq!(l1_got.data, b"l2-data");
    }

    #[tokio::test]
    async fn tiered_delete_removes_from_both() {
        let tc = TieredCache::new(MemoryCache::new(4096), MemoryCache::new(4096));
        tc.put("key", entry("data", "ct", 0)).await;
        tokio::task::yield_now().await;

        assert_eq!(tc.l1.size().await, 1);
        assert_eq!(tc.l2.size().await, 1);

        tc.delete("key").await;

        assert!(tc.l1.get("key").await.is_none());
        assert!(tc.l2.get("key").await.is_none());
        assert_eq!(tc.l1.size().await, 0);
        assert_eq!(tc.l2.size().await, 0);
    }

    #[tokio::test]
    async fn tiered_delete_returns_true_when_entry_exists_false_when_it_doesnt() {
        let tc = TieredCache::new(MemoryCache::new(4096), MemoryCache::new(4096));

        assert!(!tc.delete("nope").await);

        tc.put("key", entry("d", "t", 0)).await;
        assert!(tc.delete("key").await);
        assert!(!tc.delete("key").await);
    }

    #[tokio::test]
    async fn tiered_clear_empties_both() {
        let tc = TieredCache::new(MemoryCache::new(4096), MemoryCache::new(4096));
        tc.put("a", entry("1", "t", 0)).await;
        tc.put("b", entry("2", "t", 0)).await;
        tokio::task::yield_now().await;

        assert_eq!(tc.l1.size().await, 2);
        assert_eq!(tc.l2.size().await, 2);

        tc.clear().await;

        assert_eq!(tc.l1.size().await, 0);
        assert_eq!(tc.l2.size().await, 0);
        assert!(tc.l1.get("a").await.is_none());
        assert!(tc.l2.get("a").await.is_none());
    }

    #[tokio::test]
    async fn tiered_size_returns_l1_size() {
        let tc = TieredCache::new(MemoryCache::new(4096), MemoryCache::new(4096));
        assert_eq!(tc.size().await, 0);

        tc.put("a", entry("1", "t", 0)).await;
        assert_eq!(tc.size().await, 1);
        tokio::task::yield_now().await;

        // Put directly into L2 — tiered size should still reflect L1 only.
        tc.l2.put("extra", entry("x", "t", 0)).await;
        assert_eq!(tc.size().await, 1);
        assert_eq!(tc.l2.size().await, 2);
    }
}
