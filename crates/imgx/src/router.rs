//! URL routing. Ported from src/router.zig, then migrated to Cloudflare's
//! exact URL convention. See docs/INVARIANTS.md INV-4 (path traversal is
//! never reachable) and INV-5 (fixed `cdn-cgi/image/` prefix, OPTIONS
//! segment comes first, source image path is the remainder). Pure logic,
//! no I/O -- testable in isolation.

use thiserror::Error;

/// The fixed prefix that precedes every image request, mirroring
/// Cloudflare's `/cdn-cgi/image/<OPTIONS>/<SOURCE-IMAGE>` convention.
const IMAGE_PREFIX: &str = "cdn-cgi/image/";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RouterError {
    #[error("path traversal detected")]
    PathTraversal,
    #[error("invalid path")]
    InvalidPath,
    #[error("empty path")]
    EmptyPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequest {
    /// The path to the source image, with the leading `/` and the
    /// `cdn-cgi/image/<OPTIONS>/` prefix stripped. May itself contain `/`.
    pub image_path: String,
    /// The options segment immediately after the `cdn-cgi/image/` prefix,
    /// when it looks like a transform string (contains `=`). `None` when
    /// that segment is absent (passthrough, no transforms).
    pub transform_string: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    ImageRequest(ImageRequest),
    Health,
    Metrics,
    Ready,
    NotFound,
}

/// Resolve a raw request path into a `Route`. Well-known paths
/// (`/health`, `/metrics`, `/ready`) are matched first. Every other
/// request must use Cloudflare's exact convention: a fixed
/// `cdn-cgi/image/` prefix, then an OPTIONS segment (comma-separated
/// `key=value` pairs), then the source image path -- the remainder of
/// the URL, which may itself contain `/`.
///
/// Decision (see docs/INVARIANTS.md INV-5): Cloudflare's format assumes
/// the segment right after the prefix is always OPTIONS and requires at
/// least one parameter. imgx relaxes this for its own passthrough use
/// case: if that segment contains no `=`, it is treated as the start of
/// the image path instead (no transforms applied) rather than rejected.
/// The strict-Cloudflare case (segment contains `=`) always parses
/// byte-for-byte per Cloudflare's rule.
///
/// This is a full breaking migration: the old trailing-options shape
/// (`/<image-path>/<transforms>`) is retired outright, not kept as a
/// fallback. Any request that does not start with `cdn-cgi/image/` is
/// `NotFound`.
pub fn resolve(path: &str) -> Route {
    let clean = match sanitize_path(path) {
        Ok(c) => c,
        Err(_) => return Route::NotFound,
    };

    if clean == "health" {
        return Route::Health;
    }
    if clean == "metrics" {
        return Route::Metrics;
    }
    if clean == "ready" {
        return Route::Ready;
    }

    let Some(rest) = clean.strip_prefix(IMAGE_PREFIX) else {
        return Route::NotFound;
    };

    if rest.is_empty() {
        return Route::NotFound;
    }

    match rest.find('/') {
        Some(sep) => {
            let first = &rest[..sep];
            let remainder = &rest[sep + 1..];

            if first.contains('=') {
                if remainder.is_empty() {
                    return Route::NotFound;
                }
                Route::ImageRequest(ImageRequest {
                    image_path: remainder.to_string(),
                    transform_string: Some(first.to_string()),
                })
            } else {
                Route::ImageRequest(ImageRequest {
                    image_path: rest.to_string(),
                    transform_string: None,
                })
            }
        }
        None => {
            // No further `/` after the prefix -- a single segment.
            if rest.contains('=') {
                // Options given but no source image path to apply them to.
                return Route::NotFound;
            }
            Route::ImageRequest(ImageRequest {
                image_path: rest.to_string(),
                transform_string: None,
            })
        }
    }
}

/// Sanitize a URL path for safe use as an origin/cache key: strips the
/// leading `/`, rejects `..` traversal (literal and percent-encoded),
/// null bytes, embedded absolute paths (`//`), and empty paths.
pub fn sanitize_path(path: &str) -> Result<&str, RouterError> {
    if path.contains('\0') {
        return Err(RouterError::InvalidPath);
    }

    let stripped = path.strip_prefix('/').unwrap_or(path);

    if stripped.is_empty() {
        return Err(RouterError::EmptyPath);
    }

    if stripped.starts_with('/') {
        return Err(RouterError::InvalidPath);
    }

    if contains_traversal(stripped) || contains_encoded_traversal(stripped) {
        return Err(RouterError::PathTraversal);
    }

    Ok(stripped)
}

/// `true` when `image_path` is an absolute remote-URL source (Cloudflare
/// parity gap 2, docs/CLOUDFLARE_PARITY.md) rather than a relative path on
/// the configured origin: it starts with `http://` or `https://`,
/// case-insensitive on the scheme. Detection only -- callers decide
/// whether to actually allow/fetch it (`IMGX_ALLOW_REMOTE_SOURCES`,
/// see docs/INVARIANTS.md INV-14).
pub fn is_absolute_url_source(image_path: &str) -> bool {
    starts_with_ignore_ascii_case(image_path, "http://")
        || starts_with_ignore_ascii_case(image_path, "https://")
}

fn starts_with_ignore_ascii_case(s: &str, prefix: &str) -> bool {
    s.len() >= prefix.len() && s.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// `true` when `path` contains a `..` traversal component: exact `..`,
/// `../` prefix, `/..` suffix, or `/../` anywhere.
fn contains_traversal(path: &str) -> bool {
    path == ".." || path.starts_with("../") || path.ends_with("/..") || path.contains("/../")
}

/// `true` when `path` contains percent-encoded sequences that could
/// bypass literal traversal/null-byte checks after URL decoding: `%2e`
/// (dot), `%2f` (slash), `%00` (null byte), case-insensitive.
fn contains_encoded_traversal(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' || i + 2 >= bytes.len() {
            i += 1;
            continue;
        }
        let hi = bytes[i + 1];
        let lo = bytes[i + 2];
        if hi == b'2' && matches!(lo, b'e' | b'E' | b'f' | b'F') {
            return true;
        }
        if hi == b'0' && lo == b'0' {
            return true;
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_endpoint() {
        assert_eq!(resolve("/health"), Route::Health);
    }

    #[test]
    fn metrics_endpoint() {
        assert_eq!(resolve("/metrics"), Route::Metrics);
    }

    #[test]
    fn ready_endpoint() {
        assert_eq!(resolve("/ready"), Route::Ready);
    }

    #[test]
    fn image_with_transforms() {
        match resolve("/cdn-cgi/image/w=400,h=300/photos/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/cat.jpg");
                assert_eq!(req.transform_string.as_deref(), Some("w=400,h=300"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn image_without_transforms() {
        match resolve("/cdn-cgi/image/photos/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/cat.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn nested_path_with_transforms() {
        match resolve("/cdn-cgi/image/w=100/a/b/c/d.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "a/b/c/d.jpg");
                assert_eq!(req.transform_string.as_deref(), Some("w=100"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn nested_path_without_transforms() {
        match resolve("/cdn-cgi/image/a/b/c/d.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "a/b/c/d.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn root_path_returns_not_found() {
        assert_eq!(resolve("/"), Route::NotFound);
    }

    #[test]
    fn empty_path_returns_not_found() {
        assert_eq!(resolve(""), Route::NotFound);
    }

    #[test]
    fn path_without_cdn_cgi_prefix_returns_not_found() {
        assert_eq!(resolve("/photos/cat.jpg"), Route::NotFound);
    }

    #[test]
    fn path_without_cdn_cgi_prefix_with_transform_shape_returns_not_found() {
        assert_eq!(resolve("/photos/cat.jpg/w=400,h=300"), Route::NotFound);
    }

    #[test]
    fn bare_cdn_cgi_image_prefix_returns_not_found() {
        assert_eq!(resolve("/cdn-cgi/image"), Route::NotFound);
        assert_eq!(resolve("/cdn-cgi/image/"), Route::NotFound);
    }

    #[test]
    fn path_traversal_is_rejected_by_sanitize_path() {
        assert_eq!(
            sanitize_path("/photos/../etc/passwd"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn path_traversal_in_resolve_returns_not_found() {
        assert_eq!(
            resolve("/cdn-cgi/image/w=100/photos/../etc/passwd"),
            Route::NotFound
        );
    }

    #[test]
    fn null_byte_is_rejected_by_sanitize_path() {
        assert_eq!(
            sanitize_path("/photos/cat\0.jpg"),
            Err(RouterError::InvalidPath)
        );
    }

    #[test]
    fn null_byte_in_resolve_returns_not_found() {
        assert_eq!(resolve("/cdn-cgi/image/photos/cat\0.jpg"), Route::NotFound);
    }

    #[test]
    fn transform_detection_segment_with_equals_is_transform() {
        match resolve("/cdn-cgi/image/quality=80/img.png") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "img.png");
                assert_eq!(req.transform_string.as_deref(), Some("quality=80"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn transform_detection_segment_without_equals_is_part_of_path() {
        match resolve("/cdn-cgi/image/photos/vacation/beach.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/vacation/beach.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn path_with_only_transforms_and_no_image_path_returns_not_found() {
        assert_eq!(resolve("/cdn-cgi/image/w=400"), Route::NotFound);
    }

    #[test]
    fn options_segment_with_equals_but_empty_remainder_returns_not_found() {
        assert_eq!(resolve("/cdn-cgi/image/w=400/"), Route::NotFound);
    }

    #[test]
    fn sanitize_path_strips_leading_slash() {
        assert_eq!(sanitize_path("/photos/cat.jpg"), Ok("photos/cat.jpg"));
    }

    #[test]
    fn sanitize_path_rejects_empty_path() {
        assert_eq!(sanitize_path(""), Err(RouterError::EmptyPath));
    }

    #[test]
    fn sanitize_path_rejects_bare_slash() {
        assert_eq!(sanitize_path("/"), Err(RouterError::EmptyPath));
    }

    #[test]
    fn sanitize_path_rejects_embedded_absolute_path() {
        assert_eq!(sanitize_path("//etc/passwd"), Err(RouterError::InvalidPath));
    }

    #[test]
    fn sanitize_path_rejects_dot_dot_at_start() {
        assert_eq!(
            sanitize_path("/../etc/passwd"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn sanitize_path_rejects_dot_dot_at_end() {
        assert_eq!(sanitize_path("/photos/.."), Err(RouterError::PathTraversal));
    }

    #[test]
    fn sanitize_path_rejects_bare_dot_dot() {
        assert_eq!(sanitize_path("/.."), Err(RouterError::PathTraversal));
    }

    #[test]
    fn sanitize_path_accepts_normal_paths() {
        assert_eq!(sanitize_path("/a/b/c/file.jpg"), Ok("a/b/c/file.jpg"));
    }

    #[test]
    fn single_segment_image_path_after_prefix() {
        match resolve("/cdn-cgi/image/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "cat.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn multiple_transform_like_segments_only_first_is_treated_as_transform() {
        match resolve("/cdn-cgi/image/a=1/b=2") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "b=2");
                assert_eq!(req.transform_string.as_deref(), Some("a=1"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn sanitize_path_rejects_percent_encoded_dot_traversal() {
        assert_eq!(
            sanitize_path("/photos/%2e%2e/etc/passwd"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn sanitize_path_rejects_uppercase_percent_encoded_dot() {
        assert_eq!(
            sanitize_path("/photos/%2E%2E/etc/passwd"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn sanitize_path_rejects_encoded_null_byte() {
        assert_eq!(
            sanitize_path("/photos/cat%00.jpg"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn sanitize_path_rejects_encoded_slash() {
        assert_eq!(
            sanitize_path("/photos%2Fcat.jpg"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn encoded_traversal_in_resolve_returns_not_found() {
        assert_eq!(
            resolve("/cdn-cgi/image/w=100/photos/%2e%2e/etc/passwd"),
            Route::NotFound
        );
    }

    // ----------------------------------------------------------------
    // Gap 2 -- arbitrary remote-URL sources (docs/CLOUDFLARE_PARITY.md):
    // detecting that a source segment is an absolute URL, distinct from
    // the relative-path router tests above. Router-level detection only
    // -- allow/deny and the actual SSRF-safe fetch live in config.rs /
    // origin/remote.rs.
    // ----------------------------------------------------------------

    #[test]
    fn is_absolute_url_source_detects_http_scheme() {
        assert!(is_absolute_url_source("http://example.com/cat.jpg"));
    }

    #[test]
    fn is_absolute_url_source_detects_https_scheme() {
        assert!(is_absolute_url_source("https://example.com/cat.jpg"));
    }

    #[test]
    fn is_absolute_url_source_is_case_insensitive_on_scheme() {
        assert!(is_absolute_url_source("HTTP://example.com/cat.jpg"));
        assert!(is_absolute_url_source("HttpS://example.com/cat.jpg"));
    }

    #[test]
    fn is_absolute_url_source_rejects_relative_path() {
        assert!(!is_absolute_url_source("photos/cat.jpg"));
    }

    #[test]
    fn is_absolute_url_source_rejects_other_schemes() {
        assert!(!is_absolute_url_source("ftp://example.com/cat.jpg"));
        assert!(!is_absolute_url_source("file:///etc/passwd"));
    }

    #[test]
    fn is_absolute_url_source_rejects_short_strings() {
        assert!(!is_absolute_url_source("http"));
        assert!(!is_absolute_url_source(""));
    }

    #[test]
    fn resolve_extracts_absolute_url_as_image_path_with_transform() {
        match resolve("/cdn-cgi/image/w=100/https://example.com/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "https://example.com/cat.jpg");
                assert_eq!(req.transform_string.as_deref(), Some("w=100"));
                assert!(is_absolute_url_source(&req.image_path));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn resolve_extracts_absolute_url_as_image_path_without_transform() {
        match resolve("/cdn-cgi/image/https://example.com/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "https://example.com/cat.jpg");
                assert_eq!(req.transform_string, None);
                assert!(is_absolute_url_source(&req.image_path));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }
}
