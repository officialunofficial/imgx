//! Origin source configuration. Ported from src/origin/source.zig.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UrlError {
    #[error("image path is empty")]
    EmptyPath,
    #[error("image path contains a traversal sequence")]
    PathTraversal,
}

/// Configuration for an origin source (where original images live).
#[derive(Debug, Clone)]
pub struct OriginSource {
    pub base_url: String,
}

impl OriginSource {
    /// Build the full URL for an image path: `{base_url}/{path}`, handling
    /// a trailing slash on `base_url` and/or a leading slash on `path`.
    ///
    /// `router::sanitize_path` (INV-4) already rejects traversal sequences
    /// before a request reaches this far, but this check is kept here too
    /// as defense-in-depth -- the origin fetch is the actual trust
    /// boundary, and it shouldn't depend solely on an upstream caller
    /// having sanitized its input correctly (mirrors `cache/r2.rs`'s
    /// `sanitize_key`, INV-10).
    pub fn build_url(&self, path: &str) -> Result<String, UrlError> {
        if path.is_empty() {
            return Err(UrlError::EmptyPath);
        }
        if path.contains("..") {
            return Err(UrlError::PathTraversal);
        }

        let base = self.base_url.strip_suffix('/').unwrap_or(&self.base_url);
        let clean_path = path.strip_prefix('/').unwrap_or(path);

        Ok(format!("{base}/{clean_path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_basic() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        assert_eq!(
            origin.build_url("photos/cat.jpg").unwrap(),
            "http://images.example.com/photos/cat.jpg"
        );
    }

    #[test]
    fn build_url_trailing_slash_on_base() {
        let origin = OriginSource {
            base_url: "http://images.example.com/".to_string(),
        };
        assert_eq!(
            origin.build_url("cat.jpg").unwrap(),
            "http://images.example.com/cat.jpg"
        );
    }

    #[test]
    fn build_url_leading_slash_on_path() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        assert_eq!(
            origin.build_url("/cat.jpg").unwrap(),
            "http://images.example.com/cat.jpg"
        );
    }

    #[test]
    fn build_url_both_slashes() {
        let origin = OriginSource {
            base_url: "http://images.example.com/".to_string(),
        };
        assert_eq!(
            origin.build_url("/cat.jpg").unwrap(),
            "http://images.example.com/cat.jpg"
        );
    }

    #[test]
    fn build_url_nested_path() {
        let origin = OriginSource {
            base_url: "http://cdn.example.com".to_string(),
        };
        assert_eq!(
            origin.build_url("a/b/c/photo.jpg").unwrap(),
            "http://cdn.example.com/a/b/c/photo.jpg"
        );
    }

    #[test]
    fn build_url_empty_path_returns_error() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        assert_eq!(origin.build_url(""), Err(UrlError::EmptyPath));
    }

    #[test]
    fn build_url_rejects_traversal_sequence() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        assert_eq!(
            origin.build_url("../../etc/passwd"),
            Err(UrlError::PathTraversal)
        );
    }

    #[test]
    fn build_url_rejects_embedded_traversal_sequence() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        assert_eq!(
            origin.build_url("photos/../../../secret.jpg"),
            Err(UrlError::PathTraversal)
        );
    }
}
