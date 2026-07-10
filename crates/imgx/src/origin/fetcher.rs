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

        let mut resp = self.http.get(&url).send().await.map_err(|e| {
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

        // Reject on a declared Content-Length before reading any body, and
        // cap accumulated bytes while streaming for origins that omit it
        // (or lie about it) -- `.bytes().await` would otherwise buffer an
        // unbounded/oversized body in full before the size check ever ran.
        if resp
            .content_length()
            .is_some_and(|len| len as usize > self.max_size)
        {
            return Err(FetchError::ResponseTooLarge);
        }

        let mut data = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| FetchError::ConnectionFailed(e.to_string()))?
        {
            data.extend_from_slice(&chunk);
            if data.len() > self.max_size {
                return Err(FetchError::ResponseTooLarge);
            }
        }

        Ok(FetchResult {
            data,
            content_type,
            status_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::method as wm_method;
    use wiremock::matchers::path as wm_path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetch_returns_not_found_on_real_404() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/missing.jpg"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let origin = OriginSource {
            base_url: server.uri(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        assert!(matches!(
            fetcher.fetch("missing.jpg").await,
            Err(FetchError::NotFound)
        ));
    }

    #[tokio::test]
    async fn fetch_returns_server_error_on_real_5xx() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/broken.jpg"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let origin = OriginSource {
            base_url: server.uri(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        assert!(matches!(
            fetcher.fetch("broken.jpg").await,
            Err(FetchError::ServerError(503))
        ));
    }

    #[tokio::test]
    async fn fetch_extracts_content_type_from_real_response_header() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/photo.webp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"webpdata".to_vec())
                    .insert_header("content-type", "image/webp"),
            )
            .mount(&server)
            .await;

        let origin = OriginSource {
            base_url: server.uri(),
        };
        let fetcher = Fetcher::new(origin, 5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("photo.webp").await.unwrap();
        assert_eq!(result.content_type, "image/webp");
        assert_eq!(result.data, b"webpdata");
    }

    /// Spawns a bare-bones single-request HTTP/1.1 server on an ephemeral
    /// port that writes `response` verbatim to the first connection it
    /// accepts, then returns its base URL. Kept alongside the wiremock
    /// tests above deliberately: wiremock's `ResponseTemplate` always
    /// derives a correct Content-Length from the body it's given, so it
    /// can't express "no Content-Length header at all" (chunked) or a
    /// declared length that doesn't match the real body -- exactly the
    /// wire-level cases the tests below need.
    async fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetch_rejects_declared_content_length_over_max_size_without_reading_body() {
        let base_url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 1000\r\n\r\n",
        )
        .await;
        let origin = OriginSource { base_url };
        let fetcher = Fetcher::new(origin, 5000, 10);
        let result = fetcher.fetch("photo.png").await;
        assert!(matches!(result, Err(FetchError::ResponseTooLarge)));
    }

    #[tokio::test]
    async fn fetch_rejects_streamed_body_over_max_size_even_without_content_length() {
        // Chunked transfer with no Content-Length header -- the only way
        // to catch this is capping the accumulated body while streaming.
        let base_url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\n\r\n\
             a\r\n0123456789\r\n0\r\n\r\n",
        )
        .await;
        let origin = OriginSource { base_url };
        let fetcher = Fetcher::new(origin, 5000, 5);
        let result = fetcher.fetch("photo.png").await;
        assert!(matches!(result, Err(FetchError::ResponseTooLarge)));
    }

    #[tokio::test]
    async fn fetch_accepts_body_within_max_size() {
        let base_url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 5\r\n\r\nhello",
        )
        .await;
        let origin = OriginSource { base_url };
        let fetcher = Fetcher::new(origin, 5000, 10);
        let result = fetcher.fetch("photo.png").await.unwrap();
        assert_eq!(result.data, b"hello");
    }

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
