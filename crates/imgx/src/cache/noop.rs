//! No-op cache. Ported from src/cache/noop.zig. Every operation is a
//! no-op — used when caching is disabled.

use super::{Cache, CacheEntry};

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopCache;

impl Cache for NoopCache {
    async fn get(&self, _key: &str) -> Option<CacheEntry> {
        None
    }

    async fn put(&self, _key: &str, _entry: CacheEntry) {}

    async fn delete(&self, _key: &str) -> bool {
        false
    }

    async fn clear(&self) {}

    async fn size(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CacheEntry {
        CacheEntry {
            data: b"hello".to_vec(),
            content_type: "text/plain".to_string(),
            created_at: 1000,
        }
    }

    #[tokio::test]
    async fn noop_get_always_returns_none() {
        let nc = NoopCache;
        assert!(nc.get("any-key").await.is_none());
        assert!(nc.get("another-key").await.is_none());
    }

    #[tokio::test]
    async fn noop_put_does_not_error() {
        let nc = NoopCache;
        nc.put("key", entry()).await;
        assert!(nc.get("key").await.is_none());
    }

    #[tokio::test]
    async fn noop_delete_returns_false() {
        let nc = NoopCache;
        assert!(!nc.delete("nonexistent").await);
        nc.put("key", entry()).await;
        assert!(!nc.delete("key").await);
    }

    #[tokio::test]
    async fn noop_size_is_always_0() {
        let nc = NoopCache;
        assert_eq!(nc.size().await, 0);
        nc.put("key", entry()).await;
        assert_eq!(nc.size().await, 0);
    }

    #[tokio::test]
    async fn noop_clear_does_not_error() {
        let nc = NoopCache;
        nc.clear().await;
        assert_eq!(nc.size().await, 0);
    }
}
