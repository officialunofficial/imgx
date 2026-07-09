//! Cache interface. Ported from src/cache/cache.zig. Zig used a manual
//! vtable (fat pointer + fn-ptr table) for dynamic dispatch; Rust's trait
//! system replaces that directly. The backend set is closed (Memory,
//! Noop, R2, Tiered), so `TieredCache` is generic over its L1/L2 backends
//! rather than using `dyn Cache` — no need for object safety here.

use std::future::Future;

pub mod memory;
pub mod noop;
pub mod tiered;

pub use memory::MemoryCache;
pub use noop::NoopCache;
pub use tiered::TieredCache;

/// A cached image blob together with metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub data: Vec<u8>,
    pub content_type: String,
    /// Unix timestamp (seconds).
    pub created_at: i64,
}

/// Cache backend interface. All methods are async since the L2 (R2)
/// backend performs network I/O; L1 (memory) implementations simply
/// don't await anything internally.
pub trait Cache: Send + Sync {
    fn get(&self, key: &str) -> impl Future<Output = Option<CacheEntry>> + Send;
    fn put(&self, key: &str, entry: CacheEntry) -> impl Future<Output = ()> + Send;
    fn delete(&self, key: &str) -> impl Future<Output = bool> + Send;
    fn clear(&self) -> impl Future<Output = ()> + Send;
    fn size(&self) -> impl Future<Output = usize> + Send;
}

/// Build a deterministic cache key from image path, transform descriptor,
/// and output format: `<path>|<transforms>|<format>`.
pub fn compute_cache_key(image_path: &str, transform_string: &str, format: &str) -> String {
    format!("{image_path}|{transform_string}|{format}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_cache_key_is_deterministic() {
        let k1 = compute_cache_key("/img/photo.jpg", "w=200,h=100", "webp");
        let k2 = compute_cache_key("/img/photo.jpg", "w=200,h=100", "webp");
        assert_eq!(k1, k2);
    }

    #[test]
    fn compute_cache_key_differs_for_different_paths() {
        let k1 = compute_cache_key("/a.jpg", "w=100", "png");
        let k2 = compute_cache_key("/b.jpg", "w=100", "png");
        assert_ne!(k1, k2);
    }

    #[test]
    fn compute_cache_key_differs_for_different_transforms() {
        let k1 = compute_cache_key("/a.jpg", "w=100", "png");
        let k2 = compute_cache_key("/a.jpg", "w=200", "png");
        assert_ne!(k1, k2);
    }

    #[test]
    fn compute_cache_key_differs_for_different_formats() {
        let k1 = compute_cache_key("/a.jpg", "w=100", "png");
        let k2 = compute_cache_key("/a.jpg", "w=100", "webp");
        assert_ne!(k1, k2);
    }

    #[test]
    fn compute_cache_key_includes_all_components_separated_by_pipe() {
        assert_eq!(
            compute_cache_key("path", "transforms", "fmt"),
            "path|transforms|fmt"
        );
    }
}
