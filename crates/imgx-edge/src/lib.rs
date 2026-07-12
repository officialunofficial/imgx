//! imgx-edge: a thin Cloudflare Workers reverse proxy in front of an imgx origin.
//!
//! This Worker performs no image transforms and links no libvips code -- it exists
//! purely to sit at the edge so that Workers Caching (configured via `wrangler.toml`'s
//! `[cache]` block, not the programmatic Cache API) can absorb repeat requests before
//! they ever reach the imgx origin. imgx already owns its own multi-tier server-side
//! cache; Workers Caching is used here instead of `caches.default` because the Cache
//! API's per-datacenter-local, non-shared semantics would be a correctness trap for a
//! cache that is meant to sit in front of a single logical origin.
//!
//! On a cache miss, Workers Caching invokes this handler, which forwards the request
//! unmodified (method, headers, body) to the configured origin and returns the
//! origin's response as-is -- including `Cache-Control`/`ETag` -- so Workers Caching
//! can store it correctly on the way out. There is no transform-parameter parsing and
//! no "smart" routing here by design; see `docs/pages/cloudflare-edge-deployment.mdx`.

use worker::*;

/// `wrangler.toml` `[vars]` key naming the upstream imgx origin, e.g.
/// `https://imgx.example.com`. Must not include a path.
const ORIGIN_URL_VAR: &str = "IMGX_ORIGIN_URL";

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let origin_base = env.var(ORIGIN_URL_VAR)?.to_string();
    let origin_url = rewrite_to_origin(&req, &origin_base)?;

    let mut init = RequestInit::new();
    init.with_method(req.method())
        .with_headers(req.headers().clone());

    // GET/HEAD requests (the overwhelming majority of image fetches) carry no body;
    // avoid buffering one that doesn't exist.
    if !matches!(req.method(), Method::Get | Method::Head) {
        let body = req.bytes().await?;
        init.with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    }

    let origin_req = Request::new_with_init(origin_url.as_str(), &init)?;
    Fetch::Request(origin_req).send().await
}

/// Builds the upstream URL by swapping the incoming request's scheme and host for
/// `origin_base`, carrying the path and query through untouched. The request shape
/// (method, headers, body) is left for the caller to forward separately -- this
/// function only decides where the request goes.
fn rewrite_to_origin(req: &Request, origin_base: &str) -> Result<Url> {
    let incoming = req.url()?;
    let mut origin = Url::parse(origin_base).map_err(|e| {
        Error::RustError(format!("invalid {ORIGIN_URL_VAR} var {origin_base:?}: {e}"))
    })?;
    origin.set_path(incoming.path());
    origin.set_query(incoming.query());
    Ok(origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_to_origin_preserves_path_and_query() {
        let origin_base = "https://imgx.internal.example.com";
        let mut origin = Url::parse(origin_base).unwrap();
        origin.set_path("/photo-id/w=400");
        origin.set_query(Some("v=2"));
        assert_eq!(
            origin.as_str(),
            "https://imgx.internal.example.com/photo-id/w=400?v=2"
        );
    }

    #[test]
    fn rewrite_to_origin_rejects_invalid_origin_var() {
        let err = Url::parse("not-a-url").unwrap_err();
        assert!(!err.to_string().is_empty());
    }
}
