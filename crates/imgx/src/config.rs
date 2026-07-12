//! Configuration for imgx.
//!
//! Reads environment variables prefixed `IMGX_`, falling back to the
//! legacy `ZIMGX_` prefix (with a warning) for one release while the
//! GKE deployment migrates. See docs/INVARIANTS.md INV-8.

use std::env::VarError;
use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("server port must not be 0")]
    InvalidPort,
    #[error("timeout must not be 0")]
    InvalidTimeout,
    #[error("dimension must be at least 1")]
    InvalidDimension,
    #[error("quality must be between 1 and 100")]
    InvalidQuality,
    #[error("origin base_url must start with http:// or https://")]
    InvalidUrl,
    #[error("invalid configuration value")]
    InvalidValue,
    #[error(
        "R2 origin requires endpoint, access_key_id, secret_access_key, and both bucket names to be set"
    )]
    MissingR2Config,
    #[error("server max_connections and max_request_size must not be 0")]
    InvalidServerLimit,
    #[error("cache max_size_bytes and default_ttl_seconds must not be 0 when the cache is enabled")]
    InvalidCacheLimit,
    #[error("transform max_frames and max_animated_pixels must not be 0")]
    InvalidAnimationLimit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
    pub request_timeout_ms: u32,
    pub max_request_size: usize,
    pub max_connections: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: 8080,
            host: "0.0.0.0".to_string(),
            request_timeout_ms: 30_000,
            max_request_size: 50 * 1024 * 1024,
            max_connections: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginType {
    Http,
    R2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OriginConfig {
    pub base_url: String,
    pub timeout_ms: u32,
    pub max_retries: u8,
    pub origin_type: OriginType,
    pub path_prefix: String,
    /// Opt-in (default `false`): allow the source-image segment of a
    /// request to be an absolute `http://`/`https://` URL, fetched
    /// directly instead of from the configured origin -- Cloudflare
    /// parity gap 2 (docs/CLOUDFLARE_PARITY.md). See docs/INVARIANTS.md
    /// INV-14 for the SSRF guards enforced when this is enabled.
    pub allow_remote_sources: bool,
    /// Opt-in (default `false`), independent of `allow_remote_sources`:
    /// allow `draw[].url` overlay fetching from an arbitrary
    /// `http://`/`https://` URL -- Cloudflare parity gap 11. Reuses the
    /// same SSRF-safe fetcher and guards as `allow_remote_sources`.
    pub allow_draw_overlays: bool,
}

impl Default for OriginConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:9000".to_string(),
            timeout_ms: 10_000,
            max_retries: 2,
            origin_type: OriginType::Http,
            path_prefix: String::new(),
            allow_remote_sources: false,
            allow_draw_overlays: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct R2Config {
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket_originals: String,
    pub bucket_variants: String,
}

impl R2Config {
    fn defaults() -> Self {
        Self {
            endpoint: String::new(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            bucket_originals: "originals".to_string(),
            bucket_variants: "variants".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransformConfig {
    pub max_width: u32,
    pub max_height: u32,
    pub default_quality: u8,
    pub max_pixels: u64,
    pub strip_metadata: bool,
    pub max_frames: u32,
    pub max_animated_pixels: u64,
}

impl Default for TransformConfig {
    fn default() -> Self {
        Self {
            max_width: 8192,
            max_height: 8192,
            default_quality: 80,
            max_pixels: 71_000_000,
            strip_metadata: true,
            max_frames: 100,
            max_animated_pixels: 50_000_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheConfig {
    pub enabled: bool,
    pub max_size_bytes: usize,
    pub default_ttl_seconds: u32,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_size_bytes: 512 * 1024 * 1024,
            default_ttl_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub server: ServerConfig,
    pub origin: OriginConfig,
    pub transform: TransformConfig,
    pub cache: CacheConfig,
    pub r2: R2Config,
}

impl Config {
    pub fn defaults() -> Self {
        Self {
            r2: R2Config::defaults(),
            ..Default::default()
        }
    }

    /// Load configuration from environment variables. Missing variables
    /// keep their default values. Invalid values return
    /// `ConfigError::InvalidValue`.
    pub fn load_from_env() -> Result<Self, ConfigError> {
        let mut cfg = Self::defaults();

        if let Some(v) = env_var("SERVER_PORT") {
            cfg.server.port = parse_num(&v)?;
        }
        if let Some(v) = env_var("SERVER_HOST") {
            cfg.server.host = v;
        }
        if let Some(v) = env_var("SERVER_REQUEST_TIMEOUT_MS") {
            cfg.server.request_timeout_ms = parse_num(&v)?;
        }
        if let Some(v) = env_var("SERVER_MAX_REQUEST_SIZE") {
            cfg.server.max_request_size = parse_num(&v)?;
        }
        if let Some(v) = env_var("SERVER_MAX_CONNECTIONS") {
            cfg.server.max_connections = parse_num(&v)?;
        }

        if let Some(v) = env_var("ORIGIN_BASE_URL") {
            cfg.origin.base_url = v;
        }
        if let Some(v) = env_var("ORIGIN_TIMEOUT_MS") {
            cfg.origin.timeout_ms = parse_num(&v)?;
        }
        if let Some(v) = env_var("ORIGIN_MAX_RETRIES") {
            cfg.origin.max_retries = parse_num(&v)?;
        }
        if let Some(v) = env_var("ORIGIN_PATH_PREFIX") {
            cfg.origin.path_prefix = v;
        }
        if let Some(v) = env_var("ALLOW_REMOTE_SOURCES") {
            cfg.origin.allow_remote_sources = parse_bool(&v)?;
        }
        if let Some(v) = env_var("ALLOW_DRAW_OVERLAYS") {
            cfg.origin.allow_draw_overlays = parse_bool(&v)?;
        }

        if let Some(v) = env_var("TRANSFORM_MAX_WIDTH") {
            cfg.transform.max_width = parse_num(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_MAX_HEIGHT") {
            cfg.transform.max_height = parse_num(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_DEFAULT_QUALITY") {
            cfg.transform.default_quality = parse_num(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_MAX_PIXELS") {
            cfg.transform.max_pixels = parse_num(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_STRIP_METADATA") {
            cfg.transform.strip_metadata = parse_bool(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_MAX_FRAMES") {
            cfg.transform.max_frames = parse_num(&v)?;
        }
        if let Some(v) = env_var("TRANSFORM_MAX_ANIMATED_PIXELS") {
            cfg.transform.max_animated_pixels = parse_num(&v)?;
        }

        if let Some(v) = env_var("CACHE_ENABLED") {
            cfg.cache.enabled = parse_bool(&v)?;
        }
        if let Some(v) = env_var("CACHE_MAX_SIZE_BYTES") {
            cfg.cache.max_size_bytes = parse_num(&v)?;
        }
        if let Some(v) = env_var("CACHE_DEFAULT_TTL_SECONDS") {
            cfg.cache.default_ttl_seconds = parse_num(&v)?;
        }

        if let Some(v) = env_var("ORIGIN_TYPE") {
            cfg.origin.origin_type = match v.as_str() {
                "r2" => OriginType::R2,
                "http" => OriginType::Http,
                _ => return Err(ConfigError::InvalidValue),
            };
        }

        if let Some(v) = env_var("R2_ENDPOINT") {
            cfg.r2.endpoint = v;
        }
        if let Some(v) = env_var("R2_ACCESS_KEY_ID") {
            cfg.r2.access_key_id = v;
        }
        if let Some(v) = env_var("R2_SECRET_ACCESS_KEY") {
            cfg.r2.secret_access_key = v;
        }
        if let Some(v) = env_var("R2_BUCKET_ORIGINALS") {
            cfg.r2.bucket_originals = v;
        }
        if let Some(v) = env_var("R2_BUCKET_VARIANTS") {
            cfg.r2.bucket_variants = v;
        }

        Ok(cfg)
    }

    /// Validate the configuration, returning the first invalid field
    /// encountered. See docs/INVARIANTS.md INV-8 — this is a hard gate,
    /// the server must not start with an invalid config.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.server.port == 0 {
            return Err(ConfigError::InvalidPort);
        }
        if self.server.request_timeout_ms == 0 {
            return Err(ConfigError::InvalidTimeout);
        }
        if self.origin.timeout_ms == 0 {
            return Err(ConfigError::InvalidTimeout);
        }
        if self.transform.max_width < 1 {
            return Err(ConfigError::InvalidDimension);
        }
        if self.transform.max_height < 1 {
            return Err(ConfigError::InvalidDimension);
        }
        if self.transform.default_quality < 1 || self.transform.default_quality > 100 {
            return Err(ConfigError::InvalidQuality);
        }
        if self.server.max_connections == 0 || self.server.max_request_size == 0 {
            return Err(ConfigError::InvalidServerLimit);
        }
        if self.cache.enabled
            && (self.cache.max_size_bytes == 0 || self.cache.default_ttl_seconds == 0)
        {
            return Err(ConfigError::InvalidCacheLimit);
        }
        if self.transform.max_frames == 0 || self.transform.max_animated_pixels == 0 {
            return Err(ConfigError::InvalidAnimationLimit);
        }
        if self.origin.origin_type == OriginType::Http && !has_http_scheme(&self.origin.base_url) {
            return Err(ConfigError::InvalidUrl);
        }
        if self.origin.origin_type == OriginType::R2
            && (self.r2.endpoint.is_empty()
                || self.r2.access_key_id.is_empty()
                || self.r2.secret_access_key.is_empty()
                || self.r2.bucket_originals.is_empty()
                || self.r2.bucket_variants.is_empty())
        {
            return Err(ConfigError::MissingR2Config);
        }
        Ok(())
    }
}

fn has_http_scheme(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Read `IMGX_<suffix>`, falling back to the legacy `ZIMGX_<suffix>` (with
/// a warning) for one release while the GKE deployment migrates prefixes.
fn env_var(suffix: &str) -> Option<String> {
    let primary = format!("IMGX_{suffix}");
    match std::env::var(&primary) {
        Ok(v) => Some(v),
        Err(VarError::NotPresent) => {
            let legacy = format!("ZIMGX_{suffix}");
            match std::env::var(&legacy) {
                Ok(v) => {
                    tracing::warn!(
                        legacy_var = %legacy,
                        replacement = %primary,
                        "reading configuration from a legacy ZIMGX_ environment variable; \
                         switch to IMGX_ before the fallback is removed"
                    );
                    Some(v)
                }
                Err(_) => None,
            }
        }
        Err(VarError::NotUnicode(_)) => None,
    }
}

fn parse_num<T: FromStr>(s: &str) -> Result<T, ConfigError> {
    s.parse::<T>().map_err(|_| ConfigError::InvalidValue)
}

fn parse_bool(s: &str) -> Result<bool, ConfigError> {
    match s {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(ConfigError::InvalidValue),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_returns_expected_values() {
        let cfg = Config::defaults();

        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.server.host, "0.0.0.0");
        assert_eq!(cfg.server.request_timeout_ms, 30_000);
        assert_eq!(cfg.server.max_request_size, 50 * 1024 * 1024);
        assert_eq!(cfg.server.max_connections, 256);

        assert_eq!(cfg.origin.base_url, "http://localhost:9000");
        assert_eq!(cfg.origin.timeout_ms, 10_000);
        assert_eq!(cfg.origin.max_retries, 2);

        assert_eq!(cfg.transform.max_width, 8192);
        assert_eq!(cfg.transform.max_height, 8192);
        assert_eq!(cfg.transform.default_quality, 80);
        assert_eq!(cfg.transform.max_pixels, 71_000_000);
        assert!(cfg.transform.strip_metadata);

        assert!(cfg.cache.enabled);
        assert_eq!(cfg.cache.max_size_bytes, 512 * 1024 * 1024);
        assert_eq!(cfg.cache.default_ttl_seconds, 3600);
    }

    #[test]
    fn validate_accepts_default_config() {
        assert!(Config::defaults().validate().is_ok());
    }

    #[test]
    fn defaults_have_remote_source_and_draw_overlay_fetching_disabled() {
        let cfg = Config::defaults();
        assert!(!cfg.origin.allow_remote_sources);
        assert!(!cfg.origin.allow_draw_overlays);
    }

    #[test]
    fn validate_rejects_port_0() {
        let mut cfg = Config::defaults();
        cfg.server.port = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidPort));
    }

    #[test]
    fn validate_rejects_empty_base_url() {
        let mut cfg = Config::defaults();
        cfg.origin.base_url = String::new();
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidUrl));
    }

    #[test]
    fn validate_rejects_base_url_without_http_scheme() {
        let mut cfg = Config::defaults();
        cfg.origin.base_url = "ftp://example.com".to_string();
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidUrl));
    }

    #[test]
    fn validate_rejects_base_url_with_file_scheme() {
        let mut cfg = Config::defaults();
        cfg.origin.base_url = "file:///etc/passwd".to_string();
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidUrl));
    }

    #[test]
    fn validate_accepts_https_base_url() {
        let mut cfg = Config::defaults();
        cfg.origin.base_url = "https://cdn.example.com".to_string();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_quality_0() {
        let mut cfg = Config::defaults();
        cfg.transform.default_quality = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidQuality));
    }

    #[test]
    fn validate_rejects_quality_101() {
        let mut cfg = Config::defaults();
        cfg.transform.default_quality = 101;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidQuality));
    }

    #[test]
    fn validate_accepts_quality_at_boundaries() {
        let mut cfg = Config::defaults();
        cfg.transform.default_quality = 1;
        assert!(cfg.validate().is_ok());
        cfg.transform.default_quality = 100;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_request_timeout_ms() {
        let mut cfg = Config::defaults();
        cfg.server.request_timeout_ms = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidTimeout));
    }

    #[test]
    fn validate_rejects_zero_origin_timeout_ms() {
        let mut cfg = Config::defaults();
        cfg.origin.timeout_ms = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidTimeout));
    }

    #[test]
    fn validate_rejects_zero_max_width() {
        let mut cfg = Config::defaults();
        cfg.transform.max_width = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidDimension));
    }

    #[test]
    fn validate_rejects_zero_max_height() {
        let mut cfg = Config::defaults();
        cfg.transform.max_height = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidDimension));
    }

    #[test]
    fn validate_rejects_zero_max_connections() {
        let mut cfg = Config::defaults();
        cfg.server.max_connections = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidServerLimit));
    }

    #[test]
    fn validate_rejects_zero_max_request_size() {
        let mut cfg = Config::defaults();
        cfg.server.max_request_size = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidServerLimit));
    }

    #[test]
    fn validate_rejects_zero_cache_max_size_bytes_when_cache_enabled() {
        let mut cfg = Config::defaults();
        cfg.cache.enabled = true;
        cfg.cache.max_size_bytes = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidCacheLimit));
    }

    #[test]
    fn validate_rejects_zero_cache_ttl_when_cache_enabled() {
        let mut cfg = Config::defaults();
        cfg.cache.enabled = true;
        cfg.cache.default_ttl_seconds = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidCacheLimit));
    }

    #[test]
    fn validate_accepts_zero_cache_limits_when_cache_disabled() {
        let mut cfg = Config::defaults();
        cfg.cache.enabled = false;
        cfg.cache.max_size_bytes = 0;
        cfg.cache.default_ttl_seconds = 0;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_max_frames() {
        let mut cfg = Config::defaults();
        cfg.transform.max_frames = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidAnimationLimit));
    }

    #[test]
    fn validate_rejects_zero_max_animated_pixels() {
        let mut cfg = Config::defaults();
        cfg.transform.max_animated_pixels = 0;
        assert_eq!(cfg.validate(), Err(ConfigError::InvalidAnimationLimit));
    }

    #[test]
    fn parse_bool_helper() {
        assert_eq!(parse_bool("true"), Ok(true));
        assert_eq!(parse_bool("1"), Ok(true));
        assert_eq!(parse_bool("false"), Ok(false));
        assert_eq!(parse_bool("0"), Ok(false));
        assert_eq!(parse_bool("yes"), Err(ConfigError::InvalidValue));
        assert_eq!(parse_bool(""), Err(ConfigError::InvalidValue));
    }

    #[test]
    fn parse_num_helper() {
        assert_eq!(parse_num::<u16>("3000"), Ok(3000));
        assert_eq!(parse_num::<u32>("0"), Ok(0));
        assert_eq!(
            parse_num::<u16>("not_a_number"),
            Err(ConfigError::InvalidValue)
        );
        assert_eq!(parse_num::<u16>(""), Err(ConfigError::InvalidValue));
        // Overflow: 70000 does not fit in u16
        assert_eq!(parse_num::<u16>("70000"), Err(ConfigError::InvalidValue));
    }

    // NOTE: load_from_env's "no env vars set" case and the IMGX_/ZIMGX_
    // fallback are covered by an integration-style test in
    // tests/config_env.rs, run with --test-threads=1, since they mutate
    // process-global environment state and would race under the default
    // parallel unit-test runner.
}
