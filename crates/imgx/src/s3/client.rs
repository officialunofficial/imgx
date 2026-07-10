//! S3-compatible HTTP client, authenticated via AWS Signature V4. Ported
//! from src/s3/client.zig and src/s3/signing.zig, with one deliberate
//! architecture change from the approved plan: rather than hand-rolling
//! SigV4 header signing (as Zig did), this uses `rusty-s3` to generate
//! short-lived presigned URLs and sends them with `reqwest`. Both are
//! valid SigV4 modes; presigned URLs mean the payload itself is not part
//! of the signature (Zig's header-based signing did include a payload
//! hash) — an accepted, documented tradeoff, not a security regression:
//! the request is still fully authenticated by the signature+expiry.

use std::time::Duration;

use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use thiserror::Error;

/// Presigned URLs are generated and used immediately within the same
/// request; a short expiry limits the window a leaked/logged URL (e.g. in
/// a proxy access log) stays valid for.
const PRESIGN_EXPIRY: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum S3Error {
    #[error("s3 connection failed: {0}")]
    ConnectionFailed(String),
    #[error("s3 request signing failed: {0}")]
    SigningFailed(String),
    #[error("s3 object not found")]
    NotFound,
    #[error("s3 access denied")]
    AccessDenied,
    #[error("s3 server error (status {0})")]
    ServerError(u16),
    #[error("invalid s3 endpoint: {0}")]
    InvalidEndpoint(String),
}

/// The result of a successful S3 GET operation.
#[derive(Debug, Clone)]
pub struct S3Response {
    pub data: Vec<u8>,
    pub content_type: String,
    pub status: u16,
}

/// HTTP client wrapper for authenticated S3/R2 requests.
pub struct S3Client {
    bucket: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
}

impl S3Client {
    /// Create a new S3Client. `endpoint` is the S3/R2 endpoint URL (e.g.
    /// `https://accountid.r2.cloudflarestorage.com`); `region` should be
    /// `"auto"` for R2, or an AWS region for S3.
    pub fn new(
        endpoint: &str,
        bucket_name: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
    ) -> Result<Self, S3Error> {
        let url: url::Url = endpoint
            .parse()
            .map_err(|e| S3Error::InvalidEndpoint(format!("{e}")))?;
        let bucket = Bucket::new(
            url,
            UrlStyle::Path,
            bucket_name.to_string(),
            region.to_string(),
        )
        .map_err(|e| S3Error::InvalidEndpoint(format!("{e:?}")))?;
        let credentials = Credentials::new(access_key, secret_key);
        Ok(Self {
            bucket,
            credentials,
            http: reqwest::Client::new(),
        })
    }

    /// Fetch an object by key. Returns `Ok(None)` if the object does not
    /// exist (404).
    pub async fn get_object(&self, key: &str) -> Result<Option<S3Response>, S3Error> {
        let action = rusty_s3::actions::GetObject::new(&self.bucket, Some(&self.credentials), key);
        let url = action.sign(PRESIGN_EXPIRY);

        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| S3Error::ConnectionFailed(e.to_string()))?;

        let status = resp.status().as_u16();
        if status == 404 {
            return Ok(None);
        }
        if status == 403 {
            return Err(S3Error::AccessDenied);
        }
        if status >= 500 {
            return Err(S3Error::ServerError(status));
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
            .map_err(|e| S3Error::ConnectionFailed(e.to_string()))?;

        Ok(Some(S3Response {
            data: data.to_vec(),
            content_type,
            status,
        }))
    }

    /// Upload an object. Returns `true` on success (200/201).
    pub async fn put_object(
        &self,
        key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, S3Error> {
        let action = rusty_s3::actions::PutObject::new(&self.bucket, Some(&self.credentials), key);
        let url = action.sign(PRESIGN_EXPIRY);

        let resp = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(data)
            .send()
            .await
            .map_err(|e| S3Error::ConnectionFailed(e.to_string()))?;

        check_status(resp.status().as_u16(), 200, 201)
    }

    /// Delete an object. Returns `true` on success (200/204).
    pub async fn delete_object(&self, key: &str) -> Result<bool, S3Error> {
        let action =
            rusty_s3::actions::DeleteObject::new(&self.bucket, Some(&self.credentials), key);
        let url = action.sign(PRESIGN_EXPIRY);

        let resp = self
            .http
            .delete(url)
            .send()
            .await
            .map_err(|e| S3Error::ConnectionFailed(e.to_string()))?;

        check_status(resp.status().as_u16(), 200, 204)
    }

    /// Check whether an object exists via HEAD. `true` if it exists (200).
    pub async fn head_object(&self, key: &str) -> Result<bool, S3Error> {
        let action = rusty_s3::actions::HeadObject::new(&self.bucket, Some(&self.credentials), key);
        let url = action.sign(PRESIGN_EXPIRY);

        let resp = self
            .http
            .head(url)
            .send()
            .await
            .map_err(|e| S3Error::ConnectionFailed(e.to_string()))?;

        check_status(resp.status().as_u16(), 200, 200)
    }
}

fn check_status(status: u16, success1: u16, success2: u16) -> Result<bool, S3Error> {
    if status == 403 {
        return Err(S3Error::AccessDenied);
    }
    if status >= 500 {
        return Err(S3Error::ServerError(status));
    }
    Ok(status == success1 || status == success2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mock_client(server: &MockServer) -> S3Client {
        S3Client::new(&server.uri(), "test-bucket", "auto", "key", "secret").unwrap()
    }

    #[tokio::test]
    async fn get_object_200_returns_data_and_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/photo.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"pngdata".to_vec())
                    .insert_header("content-type", "image/png"),
            )
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let resp = client.get_object("photo.png").await.unwrap().unwrap();
        assert_eq!(resp.data, b"pngdata");
        assert_eq!(resp.content_type, "image/png");
        assert_eq!(resp.status, 200);
    }

    #[tokio::test]
    async fn get_object_404_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/missing.png"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(client.get_object("missing.png").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn get_object_403_returns_access_denied() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/forbidden.png"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(matches!(
            client.get_object("forbidden.png").await,
            Err(S3Error::AccessDenied)
        ));
    }

    #[tokio::test]
    async fn get_object_5xx_returns_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/test-bucket/broken.png"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(matches!(
            client.get_object("broken.png").await,
            Err(S3Error::ServerError(503))
        ));
    }

    #[tokio::test]
    async fn put_object_200_returns_true() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/test-bucket/upload.png"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let result = client
            .put_object("upload.png", b"data".to_vec(), "image/png")
            .await;
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn put_object_403_returns_access_denied() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/test-bucket/upload.png"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let result = client
            .put_object("upload.png", b"data".to_vec(), "image/png")
            .await;
        assert!(matches!(result, Err(S3Error::AccessDenied)));
    }

    #[tokio::test]
    async fn put_object_5xx_returns_server_error() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/test-bucket/upload.png"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        let result = client
            .put_object("upload.png", b"data".to_vec(), "image/png")
            .await;
        assert!(matches!(result, Err(S3Error::ServerError(500))));
    }

    #[tokio::test]
    async fn delete_object_204_returns_true() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/test-bucket/gone.png"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(client.delete_object("gone.png").await.unwrap());
    }

    #[tokio::test]
    async fn delete_object_403_returns_access_denied() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/test-bucket/gone.png"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(matches!(
            client.delete_object("gone.png").await,
            Err(S3Error::AccessDenied)
        ));
    }

    #[tokio::test]
    async fn head_object_200_returns_true() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/test-bucket/exists.png"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(client.head_object("exists.png").await.unwrap());
    }

    #[tokio::test]
    async fn head_object_404_returns_false() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/test-bucket/missing.png"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = mock_client(&server).await;
        assert!(!client.head_object("missing.png").await.unwrap());
    }

    #[test]
    fn s3client_new_stores_configuration() {
        let client = S3Client::new(
            "https://accountid.r2.cloudflarestorage.com",
            "my-bucket",
            "auto",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        )
        .unwrap();

        assert_eq!(client.bucket.name(), "my-bucket");
        assert_eq!(client.bucket.region(), "auto");
    }

    #[test]
    fn s3client_new_rejects_invalid_endpoint() {
        let result = S3Client::new("not a url", "bucket", "auto", "key", "secret");
        assert!(result.is_err());
    }

    /// Sanity check against a well-known, published AWS SigV4 presigned-URL
    /// test vector (also used by rusty-s3's own test suite) -- confirms our
    /// Bucket/Credentials construction and presigned-GET usage produce a
    /// byte-for-byte correct signature, replacing the Zig implementation's
    /// hand-rolled-signing test vectors (which targeted header-based
    /// signing, not presigned URLs, so aren't directly portable).
    #[test]
    fn presigned_get_matches_known_aws_sigv4_vector() {
        use jiff::Timestamp;
        use rusty_s3::actions::GetObject;

        // Fri, 24 May 2013 00:00:00 GMT
        let date = Timestamp::from_second(1_369_353_600).unwrap();
        let expires_in = Duration::from_secs(86400);

        let endpoint: url::Url = "https://s3.amazonaws.com".parse().unwrap();
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::VirtualHost,
            "examplebucket",
            "us-east-1",
        )
        .unwrap();
        let credentials = Credentials::new(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );

        let action = GetObject::new(&bucket, Some(&credentials), "test.txt");
        let url = action.sign_with_time(expires_in, &date);

        let expected = "https://examplebucket.s3.amazonaws.com/test.txt?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404";
        assert_eq!(url.as_str(), expected);
    }
}
