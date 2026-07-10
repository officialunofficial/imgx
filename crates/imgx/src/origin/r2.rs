//! R2-backed origin fetcher. Ported from src/origin/r2.zig. Fetches
//! original images from an S3/R2 bucket, returning the same FetchResult
//! type as the HTTP origin fetcher so the two backends are
//! interchangeable from the caller's perspective.

use crate::origin::fetcher::{FetchError, FetchResult};
use crate::s3::{S3Client, S3Error};

pub struct R2Fetcher<'a> {
    client: &'a S3Client,
}

impl<'a> R2Fetcher<'a> {
    pub fn new(client: &'a S3Client) -> Self {
        Self { client }
    }

    /// Fetch an image from R2 by path. The path is used directly as the
    /// S3 object key (leading slash stripped).
    pub async fn fetch(&self, image_path: &str) -> Result<FetchResult, FetchError> {
        if image_path.is_empty() {
            return Err(FetchError::InvalidUrl(
                crate::origin::source::UrlError::EmptyPath,
            ));
        }

        let key = image_path.strip_prefix('/').unwrap_or(image_path);

        let resp = match self.client.get_object(key).await {
            Ok(Some(resp)) => resp,
            Ok(None) => return Err(FetchError::NotFound),
            Err(S3Error::NotFound) => return Err(FetchError::NotFound),
            Err(S3Error::AccessDenied) => return Err(FetchError::ServerError(403)),
            Err(S3Error::ServerError(status)) => return Err(FetchError::ServerError(status)),
            Err(e) => return Err(FetchError::ConnectionFailed(e.to_string())),
        };

        Ok(FetchResult {
            data: resp.data,
            content_type: resp.content_type,
            status_code: resp.status,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_client() -> S3Client {
        S3Client::new(
            "http://localhost:1234",
            "test-bucket",
            "auto",
            "test",
            "test",
        )
        .unwrap()
    }

    async fn mock_client(server: &MockServer) -> S3Client {
        S3Client::new(&server.uri(), "test-bucket", "auto", "test", "test").unwrap()
    }

    #[tokio::test]
    async fn r2fetcher_fetch_with_empty_path_returns_invalid_url() {
        let client = test_client();
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("").await;
        assert!(matches!(result, Err(FetchError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn r2fetcher_fetch_non_empty_path_returns_connection_failed() {
        let client = test_client();
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("images/photo.jpg").await;
        assert!(matches!(result, Err(FetchError::ConnectionFailed(_))));
    }

    #[tokio::test]
    async fn r2fetcher_strips_leading_slash_before_requesting_the_key() {
        let server = MockServer::start().await;
        // If the leading slash weren't stripped, the presigned URL would
        // request "/test-bucket//test.jpg" (double slash) instead, and
        // this mock (registered without the extra slash) wouldn't match.
        Mock::given(method("GET"))
            .and(path("/test-bucket/test.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("/test.jpg").await.unwrap();
        assert_eq!(result.data, b"data");
    }

    #[tokio::test]
    async fn r2fetcher_fetch_200_returns_result() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/photo.jpg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"jpegdata".to_vec())
                    .insert_header("content-type", "image/jpeg"),
            )
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("photo.jpg").await.unwrap();
        assert_eq!(result.data, b"jpegdata");
        assert_eq!(result.content_type, "image/jpeg");
    }

    #[tokio::test]
    async fn r2fetcher_maps_s3_404_to_fetch_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/missing.jpg"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let r2 = R2Fetcher::new(&client);
        assert!(matches!(
            r2.fetch("missing.jpg").await,
            Err(FetchError::NotFound)
        ));
    }

    #[tokio::test]
    async fn r2fetcher_maps_s3_access_denied_to_fetch_server_error_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/forbidden.jpg"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let r2 = R2Fetcher::new(&client);
        assert!(matches!(
            r2.fetch("forbidden.jpg").await,
            Err(FetchError::ServerError(403))
        ));
    }

    #[tokio::test]
    async fn r2fetcher_maps_s3_5xx_to_fetch_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/broken.jpg"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let r2 = R2Fetcher::new(&client);
        assert!(matches!(
            r2.fetch("broken.jpg").await,
            Err(FetchError::ServerError(503))
        ));
    }
}
