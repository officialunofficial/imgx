//! HTTP server wiring. Ported from src/server.zig, on axum/tokio instead
//! of Zig's std.http.Server + bounded thread pool (see the approved plan
//! at /Users/christopherw/.claude/plans/recursive-meandering-crescent.md).
//! CPU-bound vips work runs in `spawn_blocking` gated by a `Semaphore`
//! (permits = available_parallelism) rather than a fixed 256-thread pool
//! with an atomic connection counter; the *invariant* that survives is
//! "reject new work under saturation rather than queueing it unboundedly,"
//! not the specific mechanism.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tokio::sync::Semaphore;

use crate::cache::{self, Cache, CacheEntry, MemoryCache, NoopCache, R2Cache, TieredCache};
use crate::config::{Config, OriginType};
use crate::http::errors::HttpError;
use crate::http::response;
use crate::origin::{FetchError, FetchResult, Fetcher, OriginSource, R2Fetcher};
use crate::router::{self, ImageRequest, Route};
use crate::s3::S3Client;
use crate::transform::{params, pipeline};

/// The cache backend selected at startup based on config. A closed set
/// (Noop/Memory/Tiered), so this is an enum rather than `dyn Cache` --
/// see docs/cache/mod.rs for why the crate favors this pattern.
pub enum AppCache {
    Noop(NoopCache),
    Memory(MemoryCache),
    Tiered(TieredCache<MemoryCache, R2Cache>),
}

impl Cache for AppCache {
    async fn get(&self, key: &str) -> Option<CacheEntry> {
        match self {
            AppCache::Noop(c) => c.get(key).await,
            AppCache::Memory(c) => c.get(key).await,
            AppCache::Tiered(c) => c.get(key).await,
        }
    }

    async fn put(&self, key: &str, entry: CacheEntry) {
        match self {
            AppCache::Noop(c) => c.put(key, entry).await,
            AppCache::Memory(c) => c.put(key, entry).await,
            AppCache::Tiered(c) => c.put(key, entry).await,
        }
    }

    async fn delete(&self, key: &str) -> bool {
        match self {
            AppCache::Noop(c) => c.delete(key).await,
            AppCache::Memory(c) => c.delete(key).await,
            AppCache::Tiered(c) => c.delete(key).await,
        }
    }

    async fn clear(&self) {
        match self {
            AppCache::Noop(c) => c.clear().await,
            AppCache::Memory(c) => c.clear().await,
            AppCache::Tiered(c) => c.clear().await,
        }
    }

    async fn size(&self) -> usize {
        match self {
            AppCache::Noop(c) => c.size().await,
            AppCache::Memory(c) => c.size().await,
            AppCache::Tiered(c) => c.size().await,
        }
    }
}

/// The origin backend selected at startup based on config.
pub enum AppOrigin {
    Http(Fetcher),
    R2(S3Client),
}

impl AppOrigin {
    async fn fetch(&self, path: &str) -> Result<FetchResult, FetchError> {
        match self {
            AppOrigin::Http(f) => f.fetch(path).await,
            AppOrigin::R2(client) => R2Fetcher::new(client).fetch(path).await,
        }
    }
}

/// Statistics about the running server, exposed via /metrics.
#[derive(Default)]
pub struct ServerStats {
    pub requests_total: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
}

impl ServerStats {
    fn to_json(&self, cache_entries: usize, uptime_seconds: i64) -> String {
        format!(
            "{{\"requests_total\":{},\"cache_hits\":{},\"cache_misses\":{},\"cache_entries\":{},\"uptime_seconds\":{}}}",
            self.requests_total.load(Ordering::Relaxed),
            self.cache_hits.load(Ordering::Relaxed),
            self.cache_misses.load(Ordering::Relaxed),
            cache_entries,
            uptime_seconds,
        )
    }
}

pub struct AppState {
    pub config: Config,
    pub cache: AppCache,
    pub origin: AppOrigin,
    pub stats: ServerStats,
    pub start_time: Instant,
    pub vips_semaphore: Arc<Semaphore>,
}

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let use_r2 = cfg.origin.origin_type == OriginType::R2;

        let cache = if cfg.cache.enabled {
            if use_r2 {
                let variants_client = S3Client::new(
                    &cfg.r2.endpoint,
                    &cfg.r2.bucket_variants,
                    "auto",
                    &cfg.r2.access_key_id,
                    &cfg.r2.secret_access_key,
                )
                .expect("invalid R2 endpoint for variants bucket");
                let mc = MemoryCache::new(cfg.cache.max_size_bytes);
                let r2c = R2Cache::new(Arc::new(variants_client));
                AppCache::Tiered(TieredCache::new(mc, r2c))
            } else {
                AppCache::Memory(MemoryCache::new(cfg.cache.max_size_bytes))
            }
        } else {
            AppCache::Noop(NoopCache)
        };

        let origin = if use_r2 {
            let originals_client = S3Client::new(
                &cfg.r2.endpoint,
                &cfg.r2.bucket_originals,
                "auto",
                &cfg.r2.access_key_id,
                &cfg.r2.secret_access_key,
            )
            .expect("invalid R2 endpoint for originals bucket");
            AppOrigin::R2(originals_client)
        } else {
            let source = OriginSource {
                base_url: cfg.origin.base_url.clone(),
            };
            AppOrigin::Http(Fetcher::new(
                source,
                cfg.origin.timeout_ms,
                cfg.server.max_request_size,
            ))
        };

        let permits = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        Self {
            config: cfg,
            cache,
            origin,
            stats: ServerStats::default(),
            start_time: Instant::now(),
            vips_semaphore: Arc::new(Semaphore::new(permits)),
        }
    }
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new().fallback(handle_request).with_state(state)
}

async fn handle_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    state.stats.requests_total.fetch_add(1, Ordering::Relaxed);

    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    let accept_header = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());

    match router::resolve(uri.path()) {
        Route::Health => json_response(StatusCode::OK, "{\"status\":\"ok\"}"),
        Route::Ready => json_response(StatusCode::OK, "{\"ready\":true}"),
        Route::Metrics => {
            let cache_entries = state.cache.size().await;
            let uptime_seconds = state.start_time.elapsed().as_secs() as i64;
            let json = state.stats.to_json(cache_entries, uptime_seconds);
            json_response(StatusCode::OK, &json)
        }
        Route::NotFound => error_response(HttpError::not_found(None)),
        Route::ImageRequest(req) => {
            handle_image_request(&state, req, if_none_match, accept_header).await
        }
    }
}

async fn handle_image_request(
    state: &AppState,
    req: ImageRequest,
    if_none_match: Option<&str>,
    accept_header: Option<&str>,
) -> Response {
    let transform_string = req.transform_string.as_deref().unwrap_or("");
    let tp = match params::parse(transform_string) {
        Ok(p) => p,
        Err(_) => {
            return error_response(HttpError::bad_request(Some(
                "invalid transform parameters".to_string(),
            )))
        }
    };
    if tp.validate().is_err() {
        return error_response(HttpError::unprocessable_entity(Some(
            "transform parameters out of range".to_string(),
        )));
    }

    let format_str = tp.format.map(|f| f.as_str()).unwrap_or("auto");
    let cache_key = cache::compute_cache_key(&req.image_path, transform_string, format_str);

    if let Some(entry) = state.cache.get(&cache_key).await {
        state.stats.cache_hits.fetch_add(1, Ordering::Relaxed);
        return build_image_response(state, entry.data, entry.content_type, if_none_match);
    }
    state.stats.cache_misses.fetch_add(1, Ordering::Relaxed);

    let effective_path = strip_path_prefix(&state.config.origin.path_prefix, &req.image_path);
    let fetch_result = match state.origin.fetch(effective_path).await {
        Ok(r) => r,
        Err(FetchError::NotFound) => {
            return error_response(HttpError::not_found(Some(
                "image not found at origin".to_string(),
            )))
        }
        Err(FetchError::Timeout) => {
            return error_response(HttpError::gateway_timeout(Some(
                "origin server timed out".to_string(),
            )))
        }
        Err(FetchError::ResponseTooLarge) => {
            return error_response(HttpError::payload_too_large(Some(
                "image exceeds size limit".to_string(),
            )))
        }
        Err(_) => {
            return error_response(HttpError::bad_gateway(Some(
                "failed to fetch from origin".to_string(),
            )))
        }
    };

    let anim_cfg = pipeline::AnimConfig {
        max_frames: state.config.transform.max_frames,
        max_animated_pixels: state.config.transform.max_animated_pixels,
    };

    let Ok(permit) = Arc::clone(&state.vips_semaphore).try_acquire_owned() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "{\"error\":{\"status\":503,\"message\":\"Service Unavailable\",\"detail\":\"server at transform capacity, retry shortly\"}}",
        );
    };

    let transform_input = fetch_result.data.clone();
    let accept_owned = accept_header.map(|s| s.to_string());

    let transform_task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        pipeline::transform(
            &transform_input,
            &tp,
            accept_owned.as_deref(),
            Some(anim_cfg),
        )
    });

    match transform_task.await {
        Ok(Ok(result)) => {
            let content_type = response::content_type_from_format(result.format).to_string();
            state
                .cache
                .put(
                    &cache_key,
                    CacheEntry {
                        data: result.data.clone(),
                        content_type: content_type.clone(),
                        created_at: now_unix(),
                    },
                )
                .await;
            build_image_response(state, result.data, content_type, if_none_match)
        }
        // Transform failed (unsupported/corrupt source, vips error, or the
        // blocking task panicked): fall back to caching/serving the raw
        // fetched bytes rather than a 500, matching the Zig original.
        Ok(Err(_)) | Err(_) => {
            let ct = tp
                .format
                .map(|f| f.content_type().to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());
            state
                .cache
                .put(
                    &cache_key,
                    CacheEntry {
                        data: fetch_result.data.clone(),
                        content_type: ct.clone(),
                        created_at: now_unix(),
                    },
                )
                .await;
            build_image_response(state, fetch_result.data, ct, if_none_match)
        }
    }
}

/// Build a response from image bytes: ETag generation, 304 handling, and
/// Cache-Control headers on 200.
fn build_image_response(
    state: &AppState,
    data: Vec<u8>,
    content_type: String,
    if_none_match: Option<&str>,
) -> Response {
    let etag = response::generate_etag(&data);

    if response::should_return_304(if_none_match, &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .unwrap();
    }

    let cache_control = response::build_cache_control(state.config.cache.default_ttl_seconds, true);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::ETAG, etag)
        .header(header::VARY, "Accept")
        .body(Body::from(data))
        .unwrap()
}

/// Strip the configured path prefix from an image path. If the path
/// starts with `<prefix>/`, the prefix and separator are removed.
fn strip_path_prefix<'a>(prefix: &str, path: &'a str) -> &'a str {
    if prefix.is_empty() {
        return path;
    }
    if let Some(rest) = path.strip_prefix(prefix) {
        if let Some(r) = rest.strip_prefix('/') {
            return r;
        }
        if rest.is_empty() {
            return path;
        }
    }
    path
}

fn json_response(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn error_response(err: HttpError) -> Response {
    let status = StatusCode::from_u16(err.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    json_response(status, &err.to_json_response())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    fn test_state() -> Arc<AppState> {
        Arc::new(AppState::new(Config::defaults()))
    }

    async fn get(router: Router, path: &str) -> (StatusCode, String) {
        let req = axum::http::Request::builder()
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn health_endpoint_returns_200_with_ok_json() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "{\"status\":\"ok\"}");
    }

    #[tokio::test]
    async fn ready_endpoint_returns_200_with_ready_json() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/ready").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "{\"ready\":true}");
    }

    #[tokio::test]
    async fn metrics_endpoint_returns_200_with_json_stats() {
        let state = test_state();
        let router = build_router(Arc::clone(&state));
        let _ = get(router.clone(), "/health").await;
        let _ = get(router.clone(), "/ready").await;
        let (status, body) = get(router, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"requests_total\":3"));
    }

    #[tokio::test]
    async fn not_found_route_returns_404_json_error() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            "{\"error\":{\"status\":404,\"message\":\"Not Found\"}}"
        );
    }

    #[tokio::test]
    async fn image_request_with_invalid_transform_returns_400() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/test.jpg/banana=42").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid transform parameters"));
    }

    #[tokio::test]
    async fn image_request_with_out_of_range_transform_returns_422() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/test.jpg/w=0").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("out of range"));
    }

    #[tokio::test]
    async fn image_request_cache_miss_increments_counter_even_when_origin_unreachable() {
        let state = test_state();
        let router = build_router(Arc::clone(&state));
        // Default config's origin is http://localhost:9000, nothing is
        // listening there, so this fails at fetch -- but cache_misses
        // must still have been counted before the fetch was attempted.
        let _ = get(router, "/test.jpg").await;
        assert_eq!(state.stats.cache_misses.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn image_request_unreachable_origin_returns_502() {
        let router = build_router(test_state());
        let (status, _) = get(router, "/test.jpg").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn strip_path_prefix_strips_matching_prefix() {
        assert_eq!(strip_path_prefix("abc123", "abc123/photo-id"), "photo-id");
    }

    #[test]
    fn strip_path_prefix_returns_original_when_no_prefix_configured() {
        assert_eq!(strip_path_prefix("", "abc123/photo-id"), "abc123/photo-id");
    }

    #[test]
    fn strip_path_prefix_returns_original_when_prefix_does_not_match() {
        assert_eq!(
            strip_path_prefix("xyz", "abc123/photo-id"),
            "abc123/photo-id"
        );
    }

    #[test]
    fn strip_path_prefix_requires_slash_after_prefix() {
        assert_eq!(
            strip_path_prefix("abc", "abc123/photo-id"),
            "abc123/photo-id"
        );
    }

    #[test]
    fn strip_path_prefix_handles_nested_path_after_prefix() {
        assert_eq!(
            strip_path_prefix("account-id", "account-id/folder/image.jpg"),
            "folder/image.jpg"
        );
    }
}
