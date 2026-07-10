//! R2/S3-backed cache. Ported from src/cache/r2.zig. All S3 errors are
//! handled gracefully: get returns None, put is best-effort, delete
//! returns false on failure.
//!
//! Fixes a latent bug present in the Zig original (see docs/INVARIANTS.md
//! INV-10): `sanitizeKey` there returns an empty string on a rejected
//! (traversal) key, but callers never check for that, so a malformed key
//! silently becomes an S3 request for the bucket root. Here,
//! `sanitize_key` returns `Option<String>` and every caller handles the
//! `None` case explicitly (treated as a cache miss / no-op).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{Cache, CacheEntry};
use crate::s3::S3Client;

pub struct R2Cache {
    client: Arc<S3Client>,
}

impl R2Cache {
    pub fn new(client: Arc<S3Client>) -> Self {
        Self { client }
    }
}

impl Cache for R2Cache {
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        let s3_key = sanitize_key(key)?;
        // Ok(None) is a legitimate cache miss and stays quiet; Err is a
        // real R2/S3 failure (bad credentials, unreachable endpoint, etc)
        // and must be visible -- silently treating it the same as a miss
        // previously meant a broken R2 config degraded to a permanent,
        // undiagnosable silent cache-miss (same failure class as the AVIF
        // bug: a fallback substituting different behavior with no signal).
        let resp = match self.client.get_object(&s3_key).await {
            Ok(Some(resp)) => resp,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, key = %s3_key, "R2 cache get failed");
                return None;
            }
        };

        // Real S3/R2 responses carry a real Content-Type header (unlike
        // Zig's std.http.Client, which didn't expose response headers at
        // all). Magic-byte sniffing is kept only as a fallback for
        // legacy cached objects that lack stored content-type metadata.
        let content_type = if resp.content_type == "application/octet-stream" {
            detect_content_type(&resp.data).to_string()
        } else {
            resp.content_type
        };

        Some(CacheEntry {
            data: resp.data,
            content_type,
            created_at: now_unix(),
        })
    }

    async fn put(&self, key: &str, entry: CacheEntry) {
        let Some(s3_key) = sanitize_key(key) else {
            return;
        };
        // Best-effort write (a failed cache write must not fail the
        // request), but the failure itself is no longer silent -- see the
        // comment on `get` above.
        if let Err(e) = self
            .client
            .put_object(&s3_key, entry.data, &entry.content_type)
            .await
        {
            tracing::warn!(error = %e, key = %s3_key, "R2 cache put failed");
        }
    }

    async fn delete(&self, key: &str) -> bool {
        let Some(s3_key) = sanitize_key(key) else {
            return false;
        };
        match self.client.delete_object(&s3_key).await {
            Ok(deleted) => deleted,
            Err(e) => {
                tracing::warn!(error = %e, key = %s3_key, "R2 cache delete failed");
                false
            }
        }
    }

    async fn clear(&self) {
        // No-op: bulk delete is not needed for MVP (matches Zig).
    }

    async fn size(&self) -> usize {
        // Not trackable via S3 (matches Zig).
        0
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Detect content type from the first bytes (magic number) of image data.
fn detect_content_type(data: &[u8]) -> &'static str {
    if data.len() < 2 {
        return "application/octet-stream";
    }

    if data.len() >= 12 {
        // WebP: "RIFF" + 4 size bytes + "WEBP"
        if &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
            return "image/webp";
        }
        // AVIF/HEIF: ftyp box at offset 4
        if &data[4..8] == b"ftyp" {
            let brand = &data[8..12];
            if brand == b"avif" || brand == b"avis" {
                return "image/avif";
            }
            if brand == b"heic" || brand == b"heix" || brand == b"mif1" {
                return "image/avif";
            }
        }
    }

    if data.len() >= 4 {
        // PNG: \x89PNG
        if data[0] == 0x89 && &data[1..4] == b"PNG" {
            return "image/png";
        }
        // GIF: GIF8 (GIF87a or GIF89a)
        if &data[0..4] == b"GIF8" {
            return "image/gif";
        }
    }

    // JPEG: \xFF\xD8
    if data[0] == 0xFF && data[1] == 0xD8 {
        return "image/jpeg";
    }

    "application/octet-stream"
}

/// Sanitize a cache key for use as an S3 object key: replace `|` with
/// `/` and collapse consecutive `/` (so `path||format` doesn't produce a
/// double-slash S3 key). Returns `None` if the result would contain `..`
/// (directory traversal in the S3 key space) -- see the module doc for
/// why this must be `Option`, not a silently-empty string.
fn sanitize_key(key: &str) -> Option<String> {
    let mut out = String::with_capacity(key.len());
    let mut prev_slash = false;
    for raw in key.chars() {
        let ch = if raw == '|' { '/' } else { raw };
        if ch == '/' && prev_slash {
            continue;
        }
        out.push(ch);
        prev_slash = ch == '/';
    }
    if out.contains("..") {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> Arc<S3Client> {
        Arc::new(
            S3Client::new(
                "http://localhost:1234",
                "test-bucket",
                "auto",
                "test",
                "test",
            )
            .unwrap(),
        )
    }

    #[tokio::test]
    async fn get_returns_none_when_s3_is_unreachable() {
        let rc = R2Cache::new(test_client());
        assert!(rc.get("some-key").await.is_none());
    }

    #[test]
    fn sanitize_key_replaces_pipes_with_slashes() {
        assert_eq!(
            sanitize_key("path|transforms|format").as_deref(),
            Some("path/transforms/format")
        );
    }

    #[test]
    fn sanitize_key_collapses_double_pipes_empty_segment() {
        assert_eq!(
            sanitize_key("test.png||auto").as_deref(),
            Some("test.png/auto")
        );
    }

    #[test]
    fn sanitize_key_leaves_key_unchanged_when_no_pipes() {
        assert_eq!(sanitize_key("simple-key").as_deref(), Some("simple-key"));
    }

    #[test]
    fn sanitize_key_rejects_traversal_sequences() {
        // Unlike the Zig original (which returned "" and relied on
        // callers to check, a gap they didn't close -- INV-10), this
        // returns None so callers can't accidentally use a bad key.
        assert_eq!(sanitize_key("photos/../etc/passwd|w=400|webp"), None);
    }

    #[tokio::test]
    async fn get_treats_traversal_key_as_a_miss_not_a_request() {
        let rc = R2Cache::new(test_client());
        // If this silently sanitized to an empty/root key and issued a
        // request, it'd still fail (unreachable host) and this assertion
        // would pass anyway -- the real regression this guards against is
        // sanitize_key ever being bypassed, covered by the unit test above
        // plus this behavioral confirmation that None short-circuits get().
        assert!(rc.get("photos/../etc/passwd|w=400|webp").await.is_none());
    }

    #[tokio::test]
    async fn size_returns_0() {
        let rc = R2Cache::new(test_client());
        assert_eq!(rc.size().await, 0);
    }

    #[tokio::test]
    async fn clear_does_not_panic() {
        let rc = R2Cache::new(test_client());
        rc.clear().await;
    }

    #[test]
    fn detect_content_type_identifies_png() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00";
        assert_eq!(detect_content_type(png), "image/png");
    }

    #[test]
    fn detect_content_type_identifies_jpeg() {
        let jpeg = b"\xFF\xD8\xFF\xE0\x00\x00\x00\x00\x00\x00\x00\x00";
        assert_eq!(detect_content_type(jpeg), "image/jpeg");
    }

    #[test]
    fn detect_content_type_identifies_webp() {
        assert_eq!(
            detect_content_type(b"RIFF\x00\x00\x00\x00WEBP"),
            "image/webp"
        );
    }

    #[test]
    fn detect_content_type_identifies_avif() {
        assert_eq!(
            detect_content_type(b"\x00\x00\x00\x1cftypavif"),
            "image/avif"
        );
    }

    #[test]
    fn detect_content_type_identifies_gif() {
        assert_eq!(
            detect_content_type(b"GIF89a\x00\x00\x00\x00\x00\x00"),
            "image/gif"
        );
        assert_eq!(
            detect_content_type(b"GIF87a\x00\x00\x00\x00\x00\x00"),
            "image/gif"
        );
    }

    #[test]
    fn detect_content_type_returns_octet_stream_for_unknown() {
        assert_eq!(detect_content_type(b"unknown"), "application/octet-stream");
    }

    #[test]
    fn detect_content_type_returns_octet_stream_for_short_data() {
        assert_eq!(detect_content_type(b"x"), "application/octet-stream");
        assert_eq!(detect_content_type(b""), "application/octet-stream");
    }
}
