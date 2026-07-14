//! SSRF-safe fetcher for arbitrary remote-URL sources (Cloudflare parity
//! gap 2, docs/CLOUDFLARE_PARITY.md) and `draw[].url` overlay fetching
//! (gap 11). Both are opt-in (`IMGX_ALLOW_REMOTE_SOURCES` /
//! `IMGX_ALLOW_DRAW_OVERLAYS`, see `config.rs`) and, when enabled, both
//! reuse this one fetcher rather than duplicating the SSRF guards. See
//! docs/INVARIANTS.md INV-14 for the full guarantee this module enforces.

use std::error::Error as StdError;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use thiserror::Error;

/// Maximum number of redirects followed before giving up. Each hop is
/// re-validated against the same scheme/private-address checks as the
/// initial request -- a redirect chain is a classic SSRF bypass if only
/// the first URL is checked.
const MAX_REDIRECTS: usize = 3;

/// Shared marker text embedded in every private-address rejection --
/// both `SafeResolver`'s hostname-resolution rejection and the literal-
/// IP-literal fast path below -- so `classify_send_error` can recognize
/// either source and map it to `RemoteFetchError::PrivateAddress`.
const PRIVATE_ADDRESS_MARKER: &str = "not globally routable";

#[derive(Debug, Error)]
pub enum RemoteFetchError {
    #[error("invalid remote url: {0}")]
    InvalidUrl(String),
    #[error("only http:// and https:// urls are supported")]
    UnsupportedScheme,
    #[error("remote host resolves to a non-public address")]
    PrivateAddress,
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("remote connection failed: {0}")]
    ConnectionFailed(String),
    #[error("remote request timed out")]
    Timeout,
    #[error("remote server returned not found")]
    NotFound,
    #[error("remote server returned a server error (status {0})")]
    ServerError(u16),
    #[error("remote response exceeded the size limit")]
    ResponseTooLarge,
}

/// The result of a successful remote fetch.
#[derive(Debug, Clone)]
pub struct RemoteFetchResult {
    pub data: Vec<u8>,
    pub content_type: String,
    pub status_code: u16,
}

/// SSRF-safe HTTP client for fetching arbitrary, attacker-influenced
/// URLs. Shared by the remote-source path (gap 2) and the draw-overlay
/// fetch path (gap 11) -- both are gated by their own config flag, but
/// once either is enabled, every guard below applies unconditionally.
pub struct RemoteFetcher {
    max_size: usize,
    http: reqwest::Client,
    enforce_private_address_guard: bool,
}

impl RemoteFetcher {
    /// `timeout_ms`/`max_size` mirror `origin::Fetcher::new` -- the same
    /// per-request timeout and body-size cap, reused rather than
    /// reinvented for this fetch path. Always enforces the DNS-resolution-
    /// time private-address guard (`SafeResolver`) -- this is the only
    /// constructor used in production wiring (`server.rs`).
    pub fn new(timeout_ms: u32, max_size: usize) -> Self {
        Self::build(timeout_ms, max_size, true)
    }

    /// Test-only constructor that skips the private-address DNS guard,
    /// so tests can exercise the rest of the fetch mechanics (streaming/
    /// size-cap discipline, content-type extraction, redirect-count and
    /// redirect-scheme re-validation, error classification) against a
    /// real wiremock server bound to loopback -- which the production
    /// guard would otherwise, correctly, always reject. The private-
    /// address guard itself is covered separately: as pure unit tests
    /// against `is_globally_routable`, and as an end-to-end test using
    /// the real `new()` against a real loopback server (see the tests
    /// module below).
    #[cfg(test)]
    fn new_without_private_address_guard_for_tests(timeout_ms: u32, max_size: usize) -> Self {
        Self::build(timeout_ms, max_size, false)
    }

    fn build(timeout_ms: u32, max_size: usize, enforce_private_address_guard: bool) -> Self {
        let redirect_policy = Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            if !matches!(attempt.url().scheme(), "http" | "https") {
                return attempt.error("redirect to unsupported scheme");
            }
            // Hyper's connector skips DNS resolution entirely for a URL
            // whose host is already an IP literal (e.g. a redirect
            // straight to `http://169.254.169.254/`) -- `SafeResolver`
            // below is never even consulted for that case, so it must be
            // checked here directly too.
            if enforce_private_address_guard && literal_ip_host_is_private(attempt.url()) {
                return attempt.error(PRIVATE_ADDRESS_MARKER);
            }
            attempt.follow()
        });

        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms as u64))
            .user_agent("imgx/1.0")
            .redirect(redirect_policy);

        if enforce_private_address_guard {
            builder = builder.dns_resolver(Arc::new(SafeResolver));
        }

        let http = builder.build().expect("reqwest client builder");

        Self {
            max_size,
            http,
            enforce_private_address_guard,
        }
    }

    /// Fetch an arbitrary remote URL. Scheme is checked up front; a
    /// literal-IP host is checked directly (see `literal_ip_host_is_private`);
    /// a hostname host goes through `SafeResolver`, which performs real
    /// DNS resolution and rejects non-globally-routable addresses
    /// *after* resolving -- catching DNS-rebinding attempts a hostname-
    /// string check alone would miss. Both checks re-run on every
    /// redirect target too (see `build`'s redirect policy), since each
    /// hop is a fresh connection through the same client.
    pub async fn fetch(&self, url: &str) -> Result<RemoteFetchResult, RemoteFetchError> {
        let parsed =
            url::Url::parse(url).map_err(|e| RemoteFetchError::InvalidUrl(e.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(RemoteFetchError::UnsupportedScheme);
        }
        if self.enforce_private_address_guard && literal_ip_host_is_private(&parsed) {
            return Err(RemoteFetchError::PrivateAddress);
        }

        let mut resp = self
            .http
            .get(parsed)
            .send()
            .await
            .map_err(classify_send_error)?;

        let status_code = resp.status().as_u16();
        // A bare 3xx surviving to here means a redirect target could not
        // be safely followed and the underlying redirect middleware gave
        // up rather than erroring -- observed for a `Location` header
        // that isn't a valid http(s) URI at all (e.g. `file:///etc/passwd`),
        // which the redirect policy closure above never even gets a
        // chance to reject. Treat it the same as any other blocked
        // redirect: an error, never a followed/successful fetch.
        if (300..400).contains(&status_code) {
            return Err(RemoteFetchError::UnsupportedScheme);
        }
        if status_code == 404 {
            return Err(RemoteFetchError::NotFound);
        }
        if status_code >= 500 {
            return Err(RemoteFetchError::ServerError(status_code));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        // Same streaming-cap discipline as origin::Fetcher (INV-12): a
        // declared Content-Length over max_size is rejected before any
        // body bytes are read, and a response without (or lying about)
        // Content-Length is capped while streaming.
        if resp
            .content_length()
            .is_some_and(|len| len as usize > self.max_size)
        {
            return Err(RemoteFetchError::ResponseTooLarge);
        }

        let mut data = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| RemoteFetchError::ConnectionFailed(e.to_string()))?
        {
            data.extend_from_slice(&chunk);
            if data.len() > self.max_size {
                return Err(RemoteFetchError::ResponseTooLarge);
            }
        }

        Ok(RemoteFetchResult {
            data,
            content_type,
            status_code,
        })
    }
}

/// `true` when `url`'s host is already an IP literal (v4 or v6) that is
/// not globally routable. Hyper's connector skips DNS resolution
/// entirely for a literal-IP host, so `SafeResolver` (which only runs on
/// hostnames that actually need resolving) would never see this case --
/// this direct check is what closes that gap.
fn literal_ip_host_is_private(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => !is_globally_routable(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => !is_globally_routable(IpAddr::V6(ip)),
        _ => false,
    }
}

/// Classify a `reqwest::Error` from a `.send()` call into a
/// `RemoteFetchError`, distinguishing timeout / blocked-redirect /
/// rejected-private-address cases from a generic connection failure.
/// The private-address check is done first (checking both the error's
/// own message and its full source chain), since a rejected redirect
/// also sets `is_redirect()` -- checking that branch first would
/// otherwise misclassify a redirect-to-private-address rejection as a
/// generic "unsupported scheme" redirect error.
fn classify_send_error(e: reqwest::Error) -> RemoteFetchError {
    if e.is_timeout() {
        return RemoteFetchError::Timeout;
    }
    if error_chain_contains(&e, PRIVATE_ADDRESS_MARKER) {
        return RemoteFetchError::PrivateAddress;
    }
    if e.is_redirect() {
        if error_chain_contains(&e, "too many redirects") {
            return RemoteFetchError::TooManyRedirects;
        }
        return RemoteFetchError::UnsupportedScheme;
    }
    RemoteFetchError::ConnectionFailed(e.to_string())
}

/// `true` when `needle` appears in `e`'s own display text or anywhere in
/// its `source()` chain.
fn error_chain_contains(e: &reqwest::Error, needle: &str) -> bool {
    if e.to_string().contains(needle) {
        return true;
    }
    let mut source: Option<&(dyn StdError + 'static)> = e.source();
    while let Some(s) = source {
        if s.to_string().contains(needle) {
            return true;
        }
        source = s.source();
    }
    false
}

/// A `reqwest` DNS resolver that performs real resolution, then rejects
/// the request outright if every resolved address is non-globally-
/// routable. Running this check *after* resolution (rather than a
/// string check on the hostname) is what catches DNS rebinding: a
/// hostname like `attacker.example` that resolves to `127.0.0.1` is
/// rejected here even though the hostname itself looks unremarkable.
struct SafeResolver;

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            // Port 0 here is a placeholder -- reqwest documents that it
            // replaces port 0 with the URL's actual port (or the
            // scheme's conventional port) after resolution.
            let lookup_target = format!("{host}:0");
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host(lookup_target)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?
                .collect();

            let safe: Vec<SocketAddr> = resolved
                .into_iter()
                .filter(|addr| is_globally_routable(addr.ip()))
                .collect();

            if safe.is_empty() {
                return Err("resolved address is not globally routable".into());
            }

            let addrs: Addrs = Box::new(safe.into_iter());
            Ok(addrs)
        })
    }
}

/// `true` when `ip` is a globally routable address -- i.e. NOT in a
/// private (RFC1918), loopback, link-local, CGNAT (RFC6598), or other
/// non-globally-routable range. See docs/INVARIANTS.md INV-14.
pub fn is_globally_routable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_globally_routable(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => is_v4_globally_routable(mapped),
            None => is_v6_globally_routable(v6),
        },
    }
}

fn is_v4_globally_routable(ip: Ipv4Addr) -> bool {
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
        || ip.is_documentation()
    {
        return false;
    }
    let o = ip.octets();
    if o[0] == 0 {
        return false; // 0.0.0.0/8, "this network"
    }
    if o[0] == 100 && (o[1] & 0xC0) == 64 {
        return false; // 100.64.0.0/10, RFC 6598 CGNAT
    }
    true
}

fn is_v6_globally_routable(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_unique_local() {
        return false;
    }
    let seg0 = ip.segments()[0];
    if seg0 & 0xffc0 == 0xfe80 {
        return false; // fe80::/10, link-local
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use wiremock::matchers::{method as wm_method, path as wm_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ------------------------------------------------------------
    // is_globally_routable -- pure unit tests against synthetic
    // IpAddr values, per the task's guidance: simpler and more
    // reliable than trying to force real DNS rebinding in a test.
    // ------------------------------------------------------------

    #[test]
    fn rejects_ipv4_loopback() {
        assert!(!is_globally_routable("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_rfc1918_private_ranges() {
        assert!(!is_globally_routable("10.0.0.1".parse().unwrap()));
        assert!(!is_globally_routable("172.16.0.1".parse().unwrap()));
        assert!(!is_globally_routable("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_link_local() {
        assert!(!is_globally_routable("169.254.1.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_cgnat_range() {
        assert!(!is_globally_routable("100.64.0.1".parse().unwrap()));
        assert!(!is_globally_routable("100.127.255.255".parse().unwrap()));
    }

    #[test]
    fn accepts_ipv4_just_outside_cgnat_range() {
        assert!(is_globally_routable("100.63.255.255".parse().unwrap()));
        assert!(is_globally_routable("100.128.0.0".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_this_network_and_broadcast_and_multicast() {
        assert!(!is_globally_routable("0.0.0.0".parse().unwrap()));
        assert!(!is_globally_routable("0.1.2.3".parse().unwrap()));
        assert!(!is_globally_routable("255.255.255.255".parse().unwrap()));
        assert!(!is_globally_routable("224.0.0.1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv6_loopback_and_link_local_and_unique_local() {
        assert!(!is_globally_routable("::1".parse().unwrap()));
        assert!(!is_globally_routable("fe80::1".parse().unwrap()));
        assert!(!is_globally_routable("fc00::1".parse().unwrap()));
        assert!(!is_globally_routable("fd12:3456:789a::1".parse().unwrap()));
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_private_address() {
        // A DNS-rebinding-relevant case: a resolver could hand back an
        // IPv4-mapped IPv6 literal wrapping a private address.
        assert!(!is_globally_routable("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_globally_routable("::ffff:10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn accepts_public_ipv4_and_ipv6_addresses() {
        assert!(is_globally_routable("8.8.8.8".parse().unwrap()));
        assert!(is_globally_routable("1.1.1.1".parse().unwrap()));
        assert!(is_globally_routable(
            "2606:4700:4700::1111".parse().unwrap()
        ));
    }

    // ------------------------------------------------------------
    // RemoteFetcher -- scheme/URL validation, no network call.
    // ------------------------------------------------------------

    #[tokio::test]
    async fn fetch_rejects_non_http_scheme_without_a_network_call() {
        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("ftp://example.com/file.jpg").await;
        assert!(matches!(result, Err(RemoteFetchError::UnsupportedScheme)));
    }

    #[tokio::test]
    async fn fetch_rejects_file_scheme() {
        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("file:///etc/passwd").await;
        assert!(matches!(result, Err(RemoteFetchError::UnsupportedScheme)));
    }

    #[tokio::test]
    async fn fetch_rejects_gopher_scheme() {
        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("gopher://example.com/file.jpg").await;
        assert!(matches!(result, Err(RemoteFetchError::UnsupportedScheme)));
    }

    #[tokio::test]
    async fn fetch_rejects_invalid_url() {
        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let result = fetcher.fetch("not a url").await;
        assert!(matches!(result, Err(RemoteFetchError::InvalidUrl(_))));
    }

    // ------------------------------------------------------------
    // RemoteFetcher -- DNS-resolution-time private-address guard,
    // exercised end-to-end through the *real* production constructor
    // (`new()`) against a real loopback server. This is what proves
    // the guard is actually wired into the fetch path, not just
    // unit-tested in isolation above -- and it's also the guard that
    // protects every redirect hop, since reqwest re-resolves DNS for
    // each new host in a redirect chain through this same resolver.
    // ------------------------------------------------------------

    #[tokio::test]
    async fn fetch_rejects_url_resolving_to_loopback() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/photo.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"nope".to_vec()))
            .mount(&server)
            .await;

        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let url = format!("{}/photo.jpg", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::PrivateAddress)));
    }

    // ------------------------------------------------------------
    // RemoteFetcher -- fetch mechanics (status handling, content-type
    // extraction, size cap, redirect count/scheme re-validation).
    // These use the guard-disabled test constructor so a real wiremock
    // server (necessarily bound to loopback) can stand in for "an
    // allowed remote source" without the production guard --
    // correctly -- rejecting every test in this section outright.
    // ------------------------------------------------------------

    #[tokio::test]
    async fn fetch_succeeds_against_a_real_server() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/photo.webp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"remote-bytes".to_vec())
                    .insert_header("content-type", "image/webp"),
            )
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/photo.webp", server.uri());
        let result = fetcher.fetch(&url).await.unwrap();
        assert_eq!(result.data, b"remote-bytes");
        assert_eq!(result.content_type, "image/webp");
        assert_eq!(result.status_code, 200);
    }

    #[tokio::test]
    async fn fetch_returns_not_found_on_real_404() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/missing.jpg"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/missing.jpg", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::NotFound)));
    }

    #[tokio::test]
    async fn fetch_returns_server_error_on_real_5xx() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/broken.jpg"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/broken.jpg", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::ServerError(503))));
    }

    /// Mirrors `origin::fetcher`'s bare-bones single-request server --
    /// wiremock can't express "no Content-Length header" or a declared
    /// length that doesn't match the real body, exactly the wire-level
    /// cases the size-cap tests below need (INV-12 parity).
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
        let fetcher = RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10);
        let url = format!("{base_url}/photo.png");
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::ResponseTooLarge)));
    }

    #[tokio::test]
    async fn fetch_rejects_streamed_body_over_max_size_even_without_content_length() {
        let base_url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\n\r\n\
             a\r\n0123456789\r\n0\r\n\r\n",
        )
        .await;
        let fetcher = RemoteFetcher::new_without_private_address_guard_for_tests(5000, 5);
        let url = format!("{base_url}/photo.png");
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::ResponseTooLarge)));
    }

    #[tokio::test]
    async fn fetch_accepts_body_within_max_size() {
        let base_url = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 5\r\n\r\nhello",
        )
        .await;
        let fetcher = RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10);
        let url = format!("{base_url}/photo.png");
        let result = fetcher.fetch(&url).await.unwrap();
        assert_eq!(result.data, b"hello");
    }

    #[tokio::test]
    async fn fetch_follows_a_redirect_within_the_cap() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/start.jpg"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/final.jpg"))
            .mount(&server)
            .await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/final.jpg"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"final-bytes".to_vec()))
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/start.jpg", server.uri());
        let result = fetcher.fetch(&url).await.unwrap();
        assert_eq!(result.data, b"final-bytes");
    }

    #[tokio::test]
    async fn fetch_rejects_a_redirect_chain_longer_than_the_cap() {
        let server = MockServer::start().await;
        // Four hops, one more than MAX_REDIRECTS (3) -- must be rejected
        // rather than followed to completion.
        for i in 0..4 {
            let next = format!("/hop{}", i + 1);
            Mock::given(wm_method("GET"))
                .and(wm_path(format!("/hop{i}")))
                .respond_with(ResponseTemplate::new(302).insert_header("location", next.as_str()))
                .mount(&server)
                .await;
        }
        Mock::given(wm_method("GET"))
            .and(wm_path("/hop4"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"unreachable".to_vec()))
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/hop0", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::TooManyRedirects)));
    }

    #[tokio::test]
    async fn fetch_rejects_a_redirect_to_a_non_http_scheme() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/start.jpg"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "file:///etc/passwd"),
            )
            .mount(&server)
            .await;

        let fetcher =
            RemoteFetcher::new_without_private_address_guard_for_tests(5000, 10 * 1024 * 1024);
        let url = format!("{}/start.jpg", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::UnsupportedScheme)));
    }

    #[tokio::test]
    async fn fetch_rejects_a_redirect_to_a_private_address_when_guard_is_enabled() {
        // Uses the real production `new()` (guard enabled). The initial
        // request is itself a loopback address, so it is rejected before
        // a redirect ever happens -- demonstrating the same end-to-end
        // guarantee a "public source redirecting to a private target"
        // scenario would (every hostname resolution, first-hop or
        // redirect-hop, goes through the same `SafeResolver`). A live
        // "passes on the first hop, rejected on a later redirect" chain
        // can't be constructed in this test environment without a real,
        // publicly-routable server to redirect *from*.
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/start.jpg"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/final.jpg"))
            .mount(&server)
            .await;

        let fetcher = RemoteFetcher::new(5000, 10 * 1024 * 1024);
        let url = format!("{}/start.jpg", server.uri());
        let result = fetcher.fetch(&url).await;
        assert!(matches!(result, Err(RemoteFetchError::PrivateAddress)));
    }
}
