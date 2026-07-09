//! Environment-variable-mutating config tests, isolated from the unit
//! test suite (which runs in parallel and would race on process env).
//! Serialized within this binary via a shared lock since std::env is
//! process-global.

use std::sync::Mutex;

use imgx::config::{Config, OriginType};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_all() {
    for suffix in [
        "SERVER_PORT",
        "SERVER_HOST",
        "SERVER_REQUEST_TIMEOUT_MS",
        "SERVER_MAX_REQUEST_SIZE",
        "SERVER_MAX_CONNECTIONS",
        "ORIGIN_BASE_URL",
        "ORIGIN_TIMEOUT_MS",
        "ORIGIN_MAX_RETRIES",
        "ORIGIN_PATH_PREFIX",
        "ORIGIN_TYPE",
        "TRANSFORM_MAX_WIDTH",
        "TRANSFORM_MAX_HEIGHT",
        "TRANSFORM_DEFAULT_QUALITY",
        "TRANSFORM_MAX_PIXELS",
        "TRANSFORM_STRIP_METADATA",
        "TRANSFORM_MAX_FRAMES",
        "TRANSFORM_MAX_ANIMATED_PIXELS",
        "CACHE_ENABLED",
        "CACHE_MAX_SIZE_BYTES",
        "CACHE_DEFAULT_TTL_SECONDS",
        "R2_ENDPOINT",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
        "R2_BUCKET_ORIGINALS",
        "R2_BUCKET_VARIANTS",
    ] {
        unsafe {
            std::env::remove_var(format!("IMGX_{suffix}"));
            std::env::remove_var(format!("ZIMGX_{suffix}"));
        }
    }
}

#[test]
fn load_from_env_with_no_env_vars_returns_defaults() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_all();

    let cfg = Config::load_from_env().expect("load succeeds with no env vars");

    assert_eq!(cfg.server.port, 8080);
    assert_eq!(cfg.server.host, "0.0.0.0");
    assert_eq!(cfg.server.request_timeout_ms, 30_000);
    assert_eq!(cfg.origin.base_url, "http://localhost:9000");
    assert_eq!(cfg.origin.timeout_ms, 10_000);
    assert_eq!(cfg.origin.max_retries, 2);
    assert_eq!(cfg.transform.max_width, 8192);
    assert_eq!(cfg.transform.default_quality, 80);
    assert!(cfg.transform.strip_metadata);
    assert!(cfg.cache.enabled);
    assert_eq!(cfg.cache.default_ttl_seconds, 3600);

    clear_all();
}

#[test]
fn imgx_prefix_takes_priority_over_legacy_zimgx_prefix() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_all();

    unsafe {
        std::env::set_var("IMGX_SERVER_PORT", "9001");
        std::env::set_var("ZIMGX_SERVER_PORT", "9002");
    }

    let cfg = Config::load_from_env().expect("load succeeds");
    assert_eq!(cfg.server.port, 9001, "IMGX_ prefix must win over ZIMGX_");

    clear_all();
}

#[test]
fn legacy_zimgx_prefix_is_used_as_a_fallback() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_all();

    unsafe {
        std::env::set_var("ZIMGX_ORIGIN_TYPE", "r2");
    }

    let cfg = Config::load_from_env().expect("load succeeds");
    assert_eq!(
        cfg.origin.origin_type,
        OriginType::R2,
        "ZIMGX_ must still be honored as a fallback"
    );

    clear_all();
}
