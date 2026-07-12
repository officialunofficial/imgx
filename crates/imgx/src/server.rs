//! HTTP server wiring. Ported from src/server.zig, on axum/tokio instead
//! of Zig's std.http.Server + bounded thread pool (see the approved plan
//! at /Users/christopherw/.claude/plans/recursive-meandering-crescent.md).
//! CPU-bound vips work runs in `spawn_blocking` gated by a `Semaphore`
//! (permits = available_parallelism) rather than a fixed 256-thread pool
//! with an atomic connection counter; the *invariant* that survives is
//! "reject new work under saturation rather than queueing it unboundedly,"
//! not the specific mechanism.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::error_handling::HandleErrorLayer;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusRecorder};
use tokio::sync::Semaphore;
use tower::ServiceBuilder;

use crate::cache::{self, Cache, CacheEntry, MemoryCache, NoopCache, R2Cache, TieredCache};
use crate::config::{Config, OriginType};
use crate::http::errors::HttpError;
use crate::http::response;
use crate::origin::{FetchError, FetchResult, Fetcher, OriginSource, R2Fetcher};
use crate::router::{self, ImageRequest, Route};
use crate::s3::S3Client;
use crate::transform::params::OnErrorMode;
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

    /// The public origin URL for `onerror=redirect` (gap 7). Cloudflare's
    /// own docs restrict this feature to "the same zone" as the
    /// transform -- imgx's equivalent restriction is that it only makes
    /// sense for an `Http` origin with a real, redirectable URL; an R2
    /// origin has no such public URL, so redirect isn't offered for it
    /// (the caller falls back to the raw-bytes default in that case).
    fn redirect_url(&self, path: &str) -> Option<String> {
        match self {
            AppOrigin::Http(f) => f.origin_url(path).ok(),
            AppOrigin::R2(_) => None,
        }
    }
}

pub struct AppState {
    pub config: Config,
    pub cache: AppCache,
    pub origin: AppOrigin,
    /// A recorder scoped to this `AppState` instance, not the process-wide
    /// global one `metrics::set_global_recorder` would install. Each
    /// `AppState` (and therefore each test) gets its own isolated metric
    /// registry -- a global recorder would make counter values a shared,
    /// racy resource across every test in the binary (they run in
    /// parallel by default), since metric names aren't otherwise scoped
    /// per-instance. Emit through it via `metrics::with_local_recorder`.
    pub metrics_recorder: PrometheusRecorder,
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

        let metrics_recorder = PrometheusBuilder::new().build_recorder();
        metrics::with_local_recorder(&metrics_recorder, || {
            metrics::describe_counter!(
                "imgx_requests_total",
                "Total number of HTTP requests handled."
            );
            metrics::describe_counter!(
                "imgx_cache_hits_total",
                "Total number of image-transform cache hits."
            );
            metrics::describe_counter!(
                "imgx_cache_misses_total",
                "Total number of image-transform cache misses."
            );
            metrics::describe_gauge!(
                "imgx_cache_entries",
                "Current number of entries in the cache (0 for backends that don't track this)."
            );
            metrics::describe_gauge!("imgx_uptime_seconds", "Seconds since the server started.");
        });

        Self {
            config: cfg,
            cache,
            origin,
            metrics_recorder,
            start_time: Instant::now(),
            vips_semaphore: Arc::new(Semaphore::new(permits)),
        }
    }
}

/// Builds the app router with a connection-level concurrency cap
/// (`IMGX_SERVER_MAX_CONNECTIONS`), rejecting with 503 at saturation
/// rather than queueing unboundedly -- the direct equivalent of Zig's
/// bounded-thread-pool + atomic connection counter, just expressed as a
/// tower middleware instead of a hand-rolled accept-loop check.
pub fn build_router(state: Arc<AppState>) -> Router {
    let max_connections = state.config.server.max_connections as usize;

    Router::new()
        .fallback(handle_request)
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_overload))
                .load_shed()
                .concurrency_limit(max_connections),
        )
}

/// Converts a `load_shed` rejection (raised when at the concurrency cap)
/// into the same JSON error envelope used elsewhere.
async fn handle_overload(_: tower::BoxError) -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "{\"error\":{\"status\":503,\"message\":\"Service Unavailable\",\"detail\":\"server at connection capacity, retry shortly\"}}",
    )
}

async fn handle_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    metrics::with_local_recorder(&state.metrics_recorder, || {
        metrics::counter!("imgx_requests_total").increment(1);
    });

    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    let accept_header = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok());

    match router::resolve(uri.path()) {
        Route::Health => json_response(StatusCode::OK, "{\"status\":\"ok\"}"),
        Route::Ready => json_response(StatusCode::OK, "{\"ready\":true}"),
        Route::Metrics => {
            let cache_entries = state.cache.size().await;
            let uptime_seconds = state.start_time.elapsed().as_secs();
            metrics::with_local_recorder(&state.metrics_recorder, || {
                metrics::gauge!("imgx_cache_entries").set(cache_entries as f64);
                metrics::gauge!("imgx_uptime_seconds").set(uptime_seconds as f64);
            });
            let body = state.metrics_recorder.handle().render();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
                body,
            )
                .into_response()
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
            )));
        }
    };
    if tp.validate().is_err() {
        return error_response(HttpError::unprocessable_entity(Some(
            "transform parameters out of range".to_string(),
        )));
    }
    // params::validate()'s 1..=8192 bound is a hard FFI-safety ceiling
    // (INV-6), not operator policy. IMGX_TRANSFORM_MAX_WIDTH/MAX_HEIGHT
    // let an operator configure a *tighter* limit on top of it -- enforce
    // that here, where config is actually in scope.
    if tp
        .width
        .is_some_and(|w| w > state.config.transform.max_width)
        || tp
            .height
            .is_some_and(|h| h > state.config.transform.max_height)
    {
        return error_response(HttpError::unprocessable_entity(Some(
            "transform parameters out of range".to_string(),
        )));
    }

    let format_str = tp.format.map(|f| f.as_str()).unwrap_or("auto");
    let cache_key = cache::compute_cache_key(&req.image_path, transform_string, format_str);

    if let Some(entry) = state.cache.get(&cache_key).await {
        metrics::with_local_recorder(&state.metrics_recorder, || {
            metrics::counter!("imgx_cache_hits_total").increment(1);
        });
        return build_image_response(state, entry.data, entry.content_type, if_none_match);
    }
    metrics::with_local_recorder(&state.metrics_recorder, || {
        metrics::counter!("imgx_cache_misses_total").increment(1);
    });

    let effective_path = strip_path_prefix(&state.config.origin.path_prefix, &req.image_path);
    let fetch_result = match state.origin.fetch(effective_path).await {
        Ok(r) => r,
        Err(FetchError::NotFound) => {
            return error_response(HttpError::not_found(Some(
                "image not found at origin".to_string(),
            )));
        }
        Err(FetchError::Timeout) => {
            return error_response(HttpError::gateway_timeout(Some(
                "origin server timed out".to_string(),
            )));
        }
        Err(FetchError::ResponseTooLarge) => {
            return error_response(HttpError::payload_too_large(Some(
                "image exceeds size limit".to_string(),
            )));
        }
        Err(_) => {
            return error_response(HttpError::bad_gateway(Some(
                "failed to fetch from origin".to_string(),
            )));
        }
    };

    let transform_limits = pipeline::TransformLimits {
        max_pixels: state.config.transform.max_pixels,
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
            Some(transform_limits),
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
        // blocking task panicked): fall back to serving the raw fetched
        // bytes rather than a 500, matching the Zig original. Always
        // logged -- a silently-swallowed transform failure here previously
        // meant every request negotiating to an unsupported format (e.g.
        // AVIF on a runtime image missing vips-heif) served the untouched
        // original with no visible signal anything was wrong.
        //
        // Deliberately NOT cached under `cache_key`: that key is also what
        // a successful transform of the same request writes to, so caching
        // the passthrough fallback here would poison it for
        // `default_ttl_seconds` -- a transient failure (e.g. brief OOM)
        // would keep serving the wrong (untransformed) bytes long after
        // the underlying problem recovered, for every future identical
        // request within the TTL window.
        Ok(Err(e)) => {
            tracing::warn!(error = %e, path = %req.image_path, transform = %transform_string, "image transform failed, serving raw origin bytes");
            handle_transform_failure(state, &req, &tp, fetch_result.data, if_none_match)
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %req.image_path, transform = %transform_string, "image transform task panicked, serving raw origin bytes");
            handle_transform_failure(state, &req, &tp, fetch_result.data, if_none_match)
        }
    }
}

/// The fallback path taken when a transform fails: imgx's default
/// (`onerror` unset) is to serve the raw origin bytes as-is (INV-13 --
/// never cached under the success key). `onerror=redirect`
/// (docs/CLOUDFLARE_PARITY.md gap 7) is an additive, opt-in per-request
/// override: a 302 to the origin image URL instead, matching
/// Cloudflare's documented `onerror=redirect` behavior. Falls back to
/// the raw-bytes default if no redirectable origin URL is available
/// (e.g. an R2 origin).
fn handle_transform_failure(
    state: &AppState,
    req: &ImageRequest,
    tp: &params::TransformParams,
    raw_data: Vec<u8>,
    if_none_match: Option<&str>,
) -> Response {
    if tp.onerror == Some(OnErrorMode::Redirect) {
        let effective_path = strip_path_prefix(&state.config.origin.path_prefix, &req.image_path);
        if let Some(location) = state.origin.redirect_url(effective_path) {
            return Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, location)
                .body(Body::empty())
                .unwrap();
        }
    }
    let ct = tp
        .format
        .map(|f| f.content_type().to_string())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    build_image_response(state, raw_data, ct, if_none_match)
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

    /// Spawns an HTTP/1.1 server that answers every request on `addr`
    /// with `response` verbatim, for the lifetime of the test. Used to
    /// serve deliberately-corrupt "image" bytes so a real transform
    /// failure (not a fetch failure) can be exercised end-to-end.
    async fn serve_repeatedly(response: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
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

    /// Reads a single metric's current value out of `state`'s own
    /// (per-instance, not global) Prometheus recorder. Parses the
    /// rendered exposition text rather than tracking a parallel
    /// AtomicU64, so tests exercise the exact same code path
    /// production scraping does. A counter that has never been
    /// incremented isn't rendered at all (Prometheus registries only
    /// emit touched metrics) -- treated as 0, matching counter semantics.
    fn metric_value(state: &AppState, name: &str) -> f64 {
        let rendered = state.metrics_recorder.handle().render();
        rendered
            .lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse::<f64>().ok())
            .unwrap_or(0.0)
    }

    /// Confirms IMGX_SERVER_MAX_CONNECTIONS actually flows into the
    /// router's concurrency limiter rather than being loaded/validated
    /// and then silently ignored. max_connections=0 means the limiter
    /// never has a free permit, so every request is immediately shed --
    /// a deterministic way to prove the wiring works without needing to
    /// race concurrent in-flight requests against each other.
    #[tokio::test]
    async fn requests_are_shed_with_503_when_max_connections_is_zero() {
        let mut cfg = Config::defaults();
        cfg.server.max_connections = 0;
        let router = build_router(Arc::new(AppState::new(cfg)));
        let (status, _) = get(router, "/health").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
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
    async fn metrics_endpoint_returns_200_with_prometheus_exposition_format() {
        let state = test_state();
        let router = build_router(Arc::clone(&state));
        let _ = get(router.clone(), "/health").await;
        let _ = get(router.clone(), "/ready").await;
        let (status, body) = get(router, "/metrics").await;
        assert_eq!(status, StatusCode::OK);
        // Every route (including /metrics itself, once it's served) counts
        // as a request, so by the time this response body was rendered,
        // three earlier requests (health, ready, and this one's own count
        // at the top of handle_request) had already been recorded.
        assert!(body.contains("# TYPE imgx_requests_total counter"));
        assert!(body.contains("imgx_requests_total 3"));
        assert!(body.contains("# TYPE imgx_cache_entries gauge"));
        assert!(body.contains("# TYPE imgx_uptime_seconds gauge"));
    }

    #[tokio::test]
    async fn metrics_content_type_is_prometheus_text_format() {
        let router = build_router(test_state());
        let req = axum::http::Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.starts_with("text/plain"));
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
        let (status, body) = get(router, "/cdn-cgi/image/banana=42/test.jpg").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid transform parameters"));
    }

    #[tokio::test]
    async fn image_request_with_out_of_range_transform_returns_422() {
        let router = build_router(test_state());
        let (status, body) = get(router, "/cdn-cgi/image/w=0/test.jpg").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("out of range"));
    }

    #[tokio::test]
    async fn image_request_width_over_configured_max_returns_422_without_hitting_origin() {
        let mut cfg = Config::defaults();
        cfg.transform.max_width = 100;
        let router = build_router(Arc::new(AppState::new(cfg)));
        // w=200 is within params.rs's hard 1..=8192 FFI-safety ceiling but
        // over this deployment's configured max_width -- must be rejected
        // before an origin fetch is ever attempted (origin is unreachable
        // by default; a 502/504 here would mean the check didn't fire).
        let (status, body) = get(router, "/cdn-cgi/image/w=200/test.jpg").await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.contains("out of range"));
    }

    #[tokio::test]
    async fn image_request_width_at_configured_max_is_accepted() {
        let mut cfg = Config::defaults();
        cfg.transform.max_width = 100;
        let router = build_router(Arc::new(AppState::new(cfg)));
        let (status, _) = get(router, "/cdn-cgi/image/w=100/test.jpg").await;
        assert_ne!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn transform_failure_fallback_is_never_cached() {
        let base_url = serve_repeatedly(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 12\r\n\r\nnot an image",
        )
        .await;
        let mut cfg = Config::defaults();
        cfg.origin.base_url = base_url;
        let state = Arc::new(AppState::new(cfg));
        let router = build_router(Arc::clone(&state));

        // First request: origin returns garbage, vips fails to decode it,
        // falls back to serving the raw bytes -- a cache miss either way.
        let (status, body) = get(router.clone(), "/cdn-cgi/image/w=100/photo.jpg").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "not an image");
        assert_eq!(metric_value(&state, "imgx_cache_misses_total"), 1.0);
        assert_eq!(metric_value(&state, "imgx_cache_hits_total"), 0.0);

        // Second, identical request: if the failed transform had been
        // cached under the transform's cache key, this would be a cache
        // hit. It must still be a miss -- the fallback is never cached.
        let (status, body) = get(router, "/cdn-cgi/image/w=100/photo.jpg").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "not an image");
        assert_eq!(metric_value(&state, "imgx_cache_misses_total"), 2.0);
        assert_eq!(metric_value(&state, "imgx_cache_hits_total"), 0.0);
    }

    #[tokio::test]
    async fn image_request_cache_miss_increments_counter_even_when_origin_unreachable() {
        let state = test_state();
        let router = build_router(Arc::clone(&state));
        // Default config's origin is http://localhost:9000, nothing is
        // listening there, so this fails at fetch -- but cache_misses
        // must still have been counted before the fetch was attempted.
        let _ = get(router, "/cdn-cgi/image/test.jpg").await;
        assert_eq!(metric_value(&state, "imgx_cache_misses_total"), 1.0);
    }

    #[tokio::test]
    async fn image_request_unreachable_origin_returns_502() {
        let router = build_router(test_state());
        let (status, _) = get(router, "/cdn-cgi/image/test.jpg").await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    /// Gap 7 -- `onerror=redirect` (docs/CLOUDFLARE_PARITY.md): opt-in
    /// per-request parameter. On a failed transform, redirect to the
    /// original source URL (302) instead of imgx's default raw-bytes
    /// fallback. The default (no `onerror`) behavior is NOT changed --
    /// see the sibling test below and INV-13 in docs/INVARIANTS.md.
    #[tokio::test]
    async fn onerror_redirect_returns_302_to_origin_url_on_transform_failure() {
        let base_url = serve_repeatedly(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 12\r\n\r\nnot an image",
        )
        .await;
        let mut cfg = Config::defaults();
        cfg.origin.base_url = base_url.clone();
        let router = build_router(Arc::new(AppState::new(cfg)));

        let (status, _) = get(router, "/cdn-cgi/image/onerror=redirect/photo.jpg").await;
        assert_eq!(status, StatusCode::FOUND);
    }

    #[tokio::test]
    async fn onerror_redirect_location_header_points_at_origin_image_url() {
        let base_url = serve_repeatedly(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 12\r\n\r\nnot an image",
        )
        .await;
        let mut cfg = Config::defaults();
        cfg.origin.base_url = base_url.clone();
        let router = build_router(Arc::new(AppState::new(cfg)));

        let req = axum::http::Request::builder()
            .uri("/cdn-cgi/image/onerror=redirect/photo.jpg")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FOUND);
        let location = resp
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap();
        assert_eq!(location, format!("{base_url}/photo.jpg"));
    }

    /// Without `onerror=redirect`, a failed transform still falls back to
    /// raw bytes -- the default behavior is unchanged (INV-13).
    #[tokio::test]
    async fn without_onerror_transform_failure_still_falls_back_to_raw_bytes() {
        let base_url = serve_repeatedly(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 12\r\n\r\nnot an image",
        )
        .await;
        let mut cfg = Config::defaults();
        cfg.origin.base_url = base_url;
        let router = build_router(Arc::new(AppState::new(cfg)));

        let (status, body) = get(router, "/cdn-cgi/image/w=100/photo.jpg").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "not an image");
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
