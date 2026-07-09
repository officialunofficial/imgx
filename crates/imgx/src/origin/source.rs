//! Origin source configuration. Ported from src/origin/source.zig.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UrlError {
    #[error("image path is empty")]
    EmptyPath,
}

/// Configuration for an origin source (where original images live).
#[derive(Debug, Clone)]
pub struct OriginSource {
    pub base_url: String,
}

impl OriginSource {
    /// Build the full URL for an image path: `{base_url}/{path}`, handling
    /// a trailing slash on `base_url` and/or a leading slash on `path`.
    pub fn build_url(&self, path: &str) -> Result<String, UrlError> {
        if path.is_empty() {
            return Err(UrlError::EmptyPath);
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
}
