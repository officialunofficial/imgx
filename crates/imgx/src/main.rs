#![forbid(unsafe_code)]

use std::sync::Arc;

use imgx::config::Config;
use imgx::server::{AppState, build_router};

// jemalloc handles the cache/pipeline's alloc-heavy, multi-threaded churn
// (frequent same-sized image-buffer alloc/free across tokio worker
// threads) better than the system allocator on Linux. The unsafe global
// allocator impl lives inside tikv-jemallocator, not here -- this crate
// still forbids unsafe code itself.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cfg = Config::load_from_env().unwrap_or_else(|e| {
        tracing::error!(error = %e, "failed to load configuration from environment");
        std::process::exit(1);
    });
    if let Err(e) = cfg.validate() {
        tracing::error!(error = %e, "invalid configuration");
        std::process::exit(1);
    }

    imgx_vips::init().expect("failed to initialize libvips");

    let host = cfg.server.host.clone();
    let port = cfg.server.port;
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let state = Arc::new(AppState::new(cfg));
    let router = build_router(state);

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind to {addr}: {e}"));

    tracing::info!(%addr, %workers, "imgx listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    imgx_vips::shutdown();
}

/// Wait for Ctrl+C or SIGTERM (the signal Kubernetes sends on pod
/// termination) so in-flight requests can drain before the process exits.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl_c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("shutdown signal received, draining in-flight requests");
}
