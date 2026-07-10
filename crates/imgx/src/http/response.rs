//! HTTP response helpers: content-type mapping, ETag generation,
//! Cache-Control construction, and conditional-request (304) support.
//! Ported from src/http/response.zig. Uses xxh3 instead of Wyhash for
//! the ETag hash (see docs/INVARIANTS.md "Assumptions & non-invariants"
//! -- a changed ETag costs one extra 304->200 revalidation per client,
//! semantically safe; cache keys are unaffected since they don't use this).

use crate::transform::params::OutputFormat;

/// Metadata attached to every image response.
#[derive(Debug, Clone)]
pub struct ResponseMeta {
    pub content_type: String,
    pub cache_control: String,
    pub etag: Option<String>,
    pub vary: &'static str,
}

/// Map an OutputFormat to its MIME content-type string.
pub fn content_type_from_format(format: OutputFormat) -> &'static str {
    format.content_type()
}

/// Generate a 16-character lowercase hex ETag from the first 8192 bytes
/// of `data` (or all of it if shorter), via xxh3-64.
pub fn generate_etag(data: &[u8]) -> String {
    let limit = data.len().min(8192);
    let hash = xxhash_rust::xxh3::xxh3_64(&data[..limit]);
    format!("{hash:016x}")
}

/// Build a Cache-Control header value, e.g. `"public, max-age=3600"`.
pub fn build_cache_control(max_age: u32, is_public: bool) -> String {
    let visibility = if is_public { "public" } else { "private" };
    format!("{visibility}, max-age={max_age}")
}

/// Whether the client's `If-None-Match` value matches the current
/// response ETag (a 304 is appropriate). Handles exact match, quoted
/// ETags (`"abc123"`), and weak ETags (`W/"abc123"`).
pub fn should_return_304(request_etag: Option<&str>, response_etag: &str) -> bool {
    let Some(raw) = request_etag else {
        return false;
    };
    strip_etag_decorations(raw) == strip_etag_decorations(response_etag)
}

/// Remove the optional `W/` weak prefix and surrounding double-quotes.
fn strip_etag_decorations(etag: &str) -> &str {
    let s = etag.strip_prefix("W/").unwrap_or(etag);
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_from_format_maps_jpeg() {
        assert_eq!(content_type_from_format(OutputFormat::Jpeg), "image/jpeg");
    }

    #[test]
    fn content_type_from_format_maps_png() {
        assert_eq!(content_type_from_format(OutputFormat::Png), "image/png");
    }

    #[test]
    fn content_type_from_format_maps_webp() {
        assert_eq!(content_type_from_format(OutputFormat::Webp), "image/webp");
    }

    #[test]
    fn content_type_from_format_maps_avif() {
        assert_eq!(content_type_from_format(OutputFormat::Avif), "image/avif");
    }

    #[test]
    fn content_type_from_format_maps_gif() {
        assert_eq!(content_type_from_format(OutputFormat::Gif), "image/gif");
    }

    #[test]
    fn content_type_from_format_maps_auto_to_octet_stream() {
        assert_eq!(
            content_type_from_format(OutputFormat::Auto),
            "application/octet-stream"
        );
    }

    #[test]
    fn generate_etag_produces_consistent_output() {
        let data = b"hello world";
        assert_eq!(generate_etag(data), generate_etag(data));
    }

    #[test]
    fn generate_etag_differs_for_different_data() {
        assert_ne!(generate_etag(b"aaa"), generate_etag(b"bbb"));
    }

    #[test]
    fn generate_etag_returns_16_character_hex_string() {
        let etag = generate_etag(b"test data");
        assert_eq!(etag.len(), 16);
        assert!(
            etag.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn build_cache_control_public() {
        assert_eq!(build_cache_control(3600, true), "public, max-age=3600");
    }

    #[test]
    fn build_cache_control_private() {
        assert_eq!(build_cache_control(60, false), "private, max-age=60");
    }

    #[test]
    fn build_cache_control_zero_max_age() {
        assert_eq!(build_cache_control(0, true), "public, max-age=0");
    }

    #[test]
    fn should_return_304_exact_match_returns_true() {
        let etag = generate_etag(b"some image data");
        assert!(should_return_304(Some(&etag), &etag));
    }

    #[test]
    fn should_return_304_no_request_etag_returns_false() {
        let etag = generate_etag(b"some image data");
        assert!(!should_return_304(None, &etag));
    }

    #[test]
    fn should_return_304_mismatch_returns_false() {
        let etag_a = generate_etag(b"data a");
        let etag_b = generate_etag(b"data b");
        assert!(!should_return_304(Some(&etag_a), &etag_b));
    }

    #[test]
    fn should_return_304_quoted_etag_handling() {
        let etag = generate_etag(b"image bytes");
        let quoted = format!("\"{etag}\"");
        assert!(should_return_304(Some(&quoted), &etag));
    }

    #[test]
    fn should_return_304_weak_etag_handling() {
        let etag = generate_etag(b"image bytes");
        let weak = format!("W/\"{etag}\"");
        assert!(should_return_304(Some(&weak), &etag));
    }

    #[test]
    fn response_meta_default_vary_is_accept() {
        let meta = ResponseMeta {
            content_type: "image/png".to_string(),
            cache_control: "public, max-age=3600".to_string(),
            etag: None,
            vary: "Accept",
        };
        assert_eq!(meta.vary, "Accept");
    }
}
