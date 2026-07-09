//! URL routing. Ported from src/router.zig. See docs/INVARIANTS.md INV-4
//! (path traversal is never reachable) and INV-5 (last `/`-segment is a
//! transform string iff it contains `=`; only the last such segment
//! counts). Pure logic, no I/O -- testable in isolation.

use thiserror::Error;

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
    /// The path to the image, with leading `/` stripped. Everything
    /// before the optional transform segment.
    pub image_path: String,
    /// The last path segment if it looks like a transform string
    /// (contains `=`). `None` when the URL has no transform segment.
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
/// (`/health`, `/metrics`, `/ready`) are matched first. Everything else
/// is a potential image request; the last path segment is a transform
/// string when it contains `=`.
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

    if let Some(sep) = clean.rfind('/') {
        let prefix = &clean[..sep];
        let last = &clean[sep + 1..];

        if last.contains('=') {
            if prefix.is_empty() {
                return Route::NotFound;
            }
            return Route::ImageRequest(ImageRequest {
                image_path: prefix.to_string(),
                transform_string: Some(last.to_string()),
            });
        }

        return Route::ImageRequest(ImageRequest {
            image_path: clean.to_string(),
            transform_string: None,
        });
    }

    // No `/` in the cleaned path -- single segment.
    if clean.contains('=') {
        return Route::NotFound;
    }
    if clean.is_empty() {
        return Route::NotFound;
    }

    Route::ImageRequest(ImageRequest {
        image_path: clean.to_string(),
        transform_string: None,
    })
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
        match resolve("/photos/cat.jpg/w=400,h=300") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/cat.jpg");
                assert_eq!(req.transform_string.as_deref(), Some("w=400,h=300"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn image_without_transforms() {
        match resolve("/photos/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/cat.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn nested_path_with_transforms() {
        match resolve("/a/b/c/d.jpg/w=100") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "a/b/c/d.jpg");
                assert_eq!(req.transform_string.as_deref(), Some("w=100"));
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
    fn path_traversal_is_rejected_by_sanitize_path() {
        assert_eq!(
            sanitize_path("/photos/../etc/passwd"),
            Err(RouterError::PathTraversal)
        );
    }

    #[test]
    fn path_traversal_in_resolve_returns_not_found() {
        assert_eq!(resolve("/photos/../etc/passwd/w=100"), Route::NotFound);
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
        assert_eq!(resolve("/photos/cat\0.jpg"), Route::NotFound);
    }

    #[test]
    fn transform_detection_segment_with_equals_is_transform() {
        match resolve("/img.png/quality=80") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "img.png");
                assert_eq!(req.transform_string.as_deref(), Some("quality=80"));
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn transform_detection_segment_without_equals_is_part_of_path() {
        match resolve("/photos/vacation/beach.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "photos/vacation/beach.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn path_with_only_transforms_and_no_image_path_returns_not_found() {
        assert_eq!(resolve("/w=400"), Route::NotFound);
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
    fn single_segment_image_path() {
        match resolve("/cat.jpg") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "cat.jpg");
                assert_eq!(req.transform_string, None);
            }
            other => panic!("expected ImageRequest, got {other:?}"),
        }
    }

    #[test]
    fn multiple_transform_like_segments_only_last_is_treated_as_transform() {
        match resolve("/a=1/b=2") {
            Route::ImageRequest(req) => {
                assert_eq!(req.image_path, "a=1");
                assert_eq!(req.transform_string.as_deref(), Some("b=2"));
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
        assert_eq!(resolve("/photos/%2e%2e/etc/passwd"), Route::NotFound);
    }
}
