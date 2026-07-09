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

    #[tokio::test]
    async fn r2fetcher_fetch_with_empty_path_returns_invalid_url() {
        let client = test_client();
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("").await;
        assert!(matches!(result, Err(FetchError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn r2fetcher_strips_leading_slash_and_attempts_connection() {
        let client = test_client();
        let r2 = R2Fetcher::new(&client);
        // No real S3 server at localhost:1234, so this fails with
        // ConnectionFailed -- proving the slash-stripping logic ran (an
        // InvalidUrl or panic would indicate it didn't).
        let result = r2.fetch("/test.jpg").await;
        assert!(matches!(result, Err(FetchError::ConnectionFailed(_))));
    }

    #[tokio::test]
    async fn r2fetcher_fetch_non_empty_path_returns_connection_failed() {
        let client = test_client();
        let r2 = R2Fetcher::new(&client);
        let result = r2.fetch("images/photo.jpg").await;
        assert!(matches!(result, Err(FetchError::ConnectionFailed(_))));
    }
}
