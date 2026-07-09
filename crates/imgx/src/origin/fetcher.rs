//! Origin fetcher: HTTP client wrapper for fetching original images from
//! an origin server. Ported from src/origin/fetcher.zig. Unlike Zig's
//! std.http.Client, reqwest exposes real response headers, so
//! content_type reflects the origin's actual `Content-Type` header
//! instead of being hardcoded to `application/octet-stream`.

use std::time::Duration;

use thiserror::Error;

use super::source::{OriginSource, UrlError};

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("origin connection failed: {0}")]
    ConnectionFailed(String),
    #[error("origin request timed out")]
    Timeout,
    #[error("origin returned not found")]
    NotFound,
    #[error("origin returned a server error (status {0})")]
    ServerError(u16),
    #[error("origin response exceeded the size limit")]
    ResponseTooLarge,
    #[error("invalid origin url: {0}")]
    InvalidUrl(#[from] UrlError),
}

/// The result of a successful origin fetch.
#[derive(Debug, Clone)]
pub struct FetchResult {
    pub data: Vec<u8>,
    pub content_type: String,
    pub status_code: u16,
}

/// HTTP client wrapper that fetches images from an origin server.
pub struct Fetcher {
    origin: OriginSource,
    max_size: usize,
    http: reqwest::Client,
}

impl Fetcher {
    /// `timeout_ms` is the per-request timeout; `max_size` is the maximum
    /// response body size in bytes.
    pub fn new(origin: OriginSource, timeout_ms: u32, max_size: usize) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms as u64))
            .user_agent("imgx/1.0")
            .build()
            .expect("reqwest client builder");
        Self {
            origin,
            max_size,
            http,
        }
    }

    /// Fetch an image from the origin by path.
    pub async fn fetch(&self, image_path: &str) -> Result<FetchResult, FetchError> {
        let url = self.origin.build_url(image_path)?;

        let resp = self.http.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::ConnectionFailed(e.to_string())
            }
        })?;

        let status_code = resp.status().as_u16();
        if status_code == 404 {
            return Err(FetchError::NotFound);
        }
        if status_code >= 500 {
            return Err(FetchError::ServerError(status_code));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let data = resp
            .bytes()
            .await
            .map_err(|e| FetchError::ConnectionFailed(e.to_string()))?;
        if data.len() > self.max_size {
            return Err(FetchError::ResponseTooLarge);
        }

        Ok(FetchResult {
            data: data.to_vec(),
            content_type,
            status_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetcher_new_stores_configuration() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        assert_eq!(fetcher.max_size, 10 * 1024 * 1024);
        assert_eq!(fetcher.origin.base_url, "http://images.example.com");
    }

    #[test]
    fn fetcher_url_building_through_origin() {
        // Exercises the same OriginSource::build_url code path used inside
        // fetch(), without requiring a real HTTP connection.
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        assert_eq!(
            fetcher.origin.build_url("photos/cat.jpg").unwrap(),
            "http://images.example.com/photos/cat.jpg"
        );
    }

    #[test]
    fn fetcher_url_building_with_slash_normalization() {
        let origin = OriginSource {
            base_url: "http://cdn.example.com/".to_string(),
        };
        let fetcher = Fetcher::new(origin, 3000, 5 * 1024 * 1024);
        assert_eq!(
            fetcher.origin.build_url("/images/photo.png").unwrap(),
            "http://cdn.example.com/images/photo.png"
        );
    }

    #[tokio::test]
    async fn fetcher_fetch_returns_invalid_url_for_empty_path() {
        let origin = OriginSource {
            base_url: "http://images.example.com".to_string(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("").await;
        assert!(matches!(
            result,
            Err(FetchError::InvalidUrl(UrlError::EmptyPath))
        ));
    }
}
