# imgx

A fast, single-binary image proxy and transform server. Fetches images from an HTTP or Cloudflare R2 origin, applies real-time resizing/format conversion/effects via [libvips](https://www.libvips.org/), and serves the result with caching, ETag support, and automatic content negotiation.

Built with [Rust](https://www.rust-lang.org/) and libvips. Runs as a single binary with no runtime dependencies beyond libvips.

## Quick Start

### Docker (recommended)

```sh
docker run -p 8080:8080 \
  -e IMGX_ORIGIN_BASE_URL=https://your-image-origin.com \
  ghcr.io/officialunofficial/imgx:latest
```

### Build from source

Requires Rust (stable) and libvips 8.14+.

```sh
cargo build --release -p imgx
./target/release/imgx
```

## URL Format

```
GET /cdn-cgi/image/<options>/<image-path>
```

imgx uses Cloudflare's exact `/cdn-cgi/image/<OPTIONS>/<SOURCE-IMAGE>` convention: a fixed `cdn-cgi/image/` prefix, then an OPTIONS segment, then the source image path. The segment right after the prefix is treated as options when it contains `=`; otherwise it's the start of a transform-less image path. Options are comma-separated `key=value` pairs.

### Examples

```
# Resize to 400px wide, auto-negotiate format
/cdn-cgi/image/w=400/photos/hero.jpg

# Resize to 800x600, convert to WebP at quality 85
/cdn-cgi/image/w=800,h=600,f=webp,q=85/photos/hero.jpg

# Cover crop with smart gravity, 2x DPR
/cdn-cgi/image/w=400,h=400,fit=cover,g=smart,dpr=2/photos/hero.jpg

# Apply blur effect
/cdn-cgi/image/blur=3.0/photos/hero.jpg

# Animated GIF resized, preserved as animated WebP
/cdn-cgi/image/w=64/photos/spinner.gif

# Extract frame 0 as static PNG
/cdn-cgi/image/frame=0,f=png/photos/spinner.gif

# Strip animation, serve first frame only
/cdn-cgi/image/anim=false/photos/spinner.gif

# Original image, no transforms
/cdn-cgi/image/photos/hero.jpg
```

## Transform Parameters

| Param | Description | Values | Default |
|-------|-------------|--------|---------|
| `w` | Width (px) | 1-8192 | - |
| `h` | Height (px) | 1-8192 | - |
| `q` | Quality | 1-100 | 80 |
| `f` | Output format | `jpeg`, `png`, `webp`, `avif`, `gif`, `auto` | auto (negotiated) |
| `fit` | Resize mode | `contain`, `cover`, `fill`, `inside`, `outside`, `pad` | `contain` |
| `g` | Crop gravity | `center`, `north`, `south`, `east`, `west`, `ne`, `nw`, `se`, `sw`, `smart`, `attention` | `center` |
| `sharpen` | Sharpen sigma | 0.0-10.0 | - |
| `blur` | Gaussian blur sigma | 0.1-250.0 | - |
| `dpr` | Device pixel ratio | 1.0-5.0 | 1.0 |
| `anim` | Animation mode | `true`, `false`, `auto`, `static`, `animate` | `auto` (`true`) |
| `frame` | Extract single frame | 0-999 | - |

See [docs/pages/transforms.mdx](docs/pages/transforms.mdx) for full details.

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `IMGX_SERVER_PORT` | Listen port | `8080` |
| `IMGX_SERVER_HOST` | Bind address | `0.0.0.0` |
| `IMGX_ORIGIN_TYPE` | Origin backend | `http` |
| `IMGX_ORIGIN_BASE_URL` | HTTP origin base URL | `http://localhost:9000` |
| `IMGX_CACHE_ENABLED` | Enable in-memory cache | `true` |
| `IMGX_CACHE_MAX_SIZE_BYTES` | Max cache size | `536870912` (512MB) |
| `IMGX_R2_ENDPOINT` | R2/S3 endpoint URL | - |
| `IMGX_R2_ACCESS_KEY_ID` | R2/S3 access key | - |
| `IMGX_R2_SECRET_ACCESS_KEY` | R2/S3 secret key | - |

The legacy `ZIMGX_` prefix is still read as a fallback for one release during the migration from zimgx (Zig) to imgx (Rust). See [docs/pages/configuration.mdx](docs/pages/configuration.mdx) for the full reference.

## Endpoints

| Path | Description |
|------|-------------|
| `GET /health` | Health check &mdash; `{"status":"ok"}` |
| `GET /ready` | Readiness probe &mdash; `{"ready":true}` |
| `GET /metrics` | Prometheus exposition format (requests, cache hits/misses, uptime) |
| `GET /cdn-cgi/image/<options>/<path>` | Image request (with optional transforms) |

## Architecture

```
                    Request
                      │
                ┌─────▼─────┐
                │   Router   │
                └─────┬─────┘
                      │
              ┌───────▼───────┐
              │  Cache Lookup │
              │  (L1 Memory)  │
              └───────┬───────┘
                  hit/│\miss
                 ┌────┘ └────┐
                 │           │
                 ▼      ┌────▼────┐
              Respond   │ L2 R2   │ (optional)
                        │ Cache   │
                        └────┬────┘
                         hit/│\miss
                        ┌────┘ └────┐
                        │           │
                        ▼      ┌────▼─────┐
                     Respond   │  Origin   │
                               │ (HTTP/R2) │
                               └────┬──────┘
                                    │
                             ┌──────▼──────┐
                             │  Transform  │
                             │  Pipeline   │
                             │ (libvips)   │
                             └──────┬──────┘
                                    │
                              ┌─────▼─────┐
                              │   Cache    │
                              │   Store    │
                              └─────┬─────┘
                                    │
                                 Respond
```

See [docs/pages/architecture.mdx](docs/pages/architecture.mdx) for full details.

## Performance

imgx is built on tokio/axum with CPU-bound libvips work dispatched via `spawn_blocking`, gated by a semaphore sized to available parallelism. Benchmarks from the prior Zig implementation are not representative of the Rust rewrite's performance profile (different concurrency model, different encoder call overhead) and have been removed pending a fresh benchmark pass; see [docs/PARITY.md](docs/PARITY.md) for the correctness parity verification done as part of the rewrite.

## Documentation

- [Configuration Reference](docs/pages/configuration.mdx) &mdash; all `IMGX_*` environment variables
- [Transform Parameters](docs/pages/transforms.mdx) &mdash; resize, format, effects
- [Deployment Guide](docs/pages/deployment.mdx) &mdash; Docker, Compose, health checks
- [Architecture](docs/pages/architecture.mdx) &mdash; system design, module map, caching
- [Invariants](docs/INVARIANTS.md) &mdash; behaviors that must survive any future changes
- [Parity Verification](docs/PARITY.md) &mdash; zimgx (Zig) → imgx (Rust) rewrite parity pass

## License

MIT
