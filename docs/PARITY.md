# zimgx (Zig) vs imgx (Rust) parity pass

Phase 8 of the rewrite plan (`/Users/christopherw/.claude/plans/recursive-meandering-crescent.md`):
run both binaries side by side against the same origin and fixtures, diff status
codes, headers, cache keys, and image dimensions.

## Method

The host's Zig toolchain (0.16) can't build the original Zig 0.15 source — a
newer, unrelated `std.io` API removed in 0.16 that `params.zig`/`bindings.zig`
depend on. The host's system linker also can't link Zig 0.15.2 itself against
this machine's macOS 26.5 SDK (self-hosted Mach-O linker predates that SDK).
Both are environment/toolchain issues, not code issues.

Worked around by building both binaries inside Docker, from the exact same
`alpine:3.20` base (vips 8.15.2), each via its own real toolchain:

- **zimgx**: the project's actual `Dockerfile`, with `apk add zig` replaced by
  an injected Zig 0.15.2 Linux build (Alpine's `edge` `zig` package had also
  drifted to 0.16; pinning to `alpine:3.20` avoided that).
- **imgx**: `cargo build --release`, same `alpine:3.20` base, same `.cargo/config.toml`
  musl fix from phase 1.

Both containers pointed at a shared local HTTP origin (`python3 -m http.server`
serving `test/fixtures/`), each on its own port, queried with matched `curl`
requests, diffing status, `content-type`, and (for image bodies) real decoded
pixel dimensions parsed from PNG/WebP/JPEG/GIF headers — not just byte length.

## Results

**20/20 test cases matched exactly** — status code, content-type, and byte-for-byte
identical response bodies (not just dimensions) for:

- `/health`, `/ready`
- Passthrough, resize, `fit=cover`, `fit=fill`
- Format conversion: WebP, PNG, JPEG
- Effects: rotate, flip, sharpen, blur
- Error paths: invalid transform (400), out-of-range (422), origin 404
- Animated GIF: passthrough, resize, `anim=static` degrade, `frame=N` extraction
  — all 4 cases byte-identical, including the two-step cover-resize workaround
  (INV-2) and GIF page-height safety check (INV-3) paths

One initial run (Rust built natively on macOS against Homebrew's vips 8.18,
compared against Zig in Alpine/vips 8.15.2) showed a mismatch on AVIF requests:
Zig fell back to `application/octet-stream` + raw origin bytes (its HEIF
encoder unavailable in that vips build), Rust produced real AVIF (macOS's vips
build has HEIF support). Rebuilding Rust against the *same* Alpine/vips 8.15.2
image reproduced Zig's exact fallback behavior byte-for-byte — confirming this
was a libvips build-capability difference between the two test environments,
not a behavioral divergence between the implementations. Both correctly
implement the "encode failed → serve raw origin bytes, uncached-format" path.

**304 conditional requests**: both implementations correctly self-revalidate
(fresh ETag → 304) and correctly return 200 on a foreign/stale ETag. ETag
*values* differ between the two by design (Wyhash vs xxh3-64, documented in
`docs/INVARIANTS.md`) — this costs at most one extra 304→200 revalidation per
client across the cutover, not a correctness issue.

## Conclusion

No behavioral divergence found between the Rust rewrite and the original Zig
implementation across static transforms, format negotiation, effects, error
handling, or animated-GIF processing (including both CRITICAL invariants,
INV-2 and INV-3). Proceeding to phase 9 (Docker/CI swap, renaming, Zig source
removal).
