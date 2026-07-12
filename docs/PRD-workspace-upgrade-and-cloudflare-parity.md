# PRD: imgx — Workspace Dependency Upgrade & Cloudflare Images Parity

**Status:** Streams A/B/C executed; parameter-parity gaps (Stream B §3.b/3.c Phase B3+) and several open questions remain for follow-up.
**Repo:** `officialunofficial/imgx` (Cargo workspace + vocs docs site)
**Author:** Fable 5 (planning). **Executor:** Sonnet 5. **Reviewer:** repo owner.

---

## 1. Executive Summary

Three work streams. **Stream A** brings every Rust crate dependency (workspace root + `crates/imgx` + `crates/imgx-vips`) and every npm devDependency (vocs docs site) to latest safe versions, keeping fmt/clippy/tests/docs-build green throughout. **Stream B** closes the gap between imgx's URL/transform surface and Cloudflare Images' real `/cdn-cgi/image/<OPTIONS>/<SOURCE>` convention — the owner chose a full breaking migration (Option B) over an additive dual-route, so imgx's primary URL scheme now matches Cloudflare's exactly. **Stream C** adds a `workers-rs` edge Worker (wrangler.toml, Workers Caching) in front of the real imgx origin as a cache/router layer — explicitly NOT a port of the transform pipeline, since libvips cannot run inside a Worker's WASM sandbox.

---

## 2. Stream A: Full Workspace Dependency Upgrade

### 2.0 Pre-flight blocker check

- Toolchain mismatch (rustc 1.94.1 vs declared `rust-version = "1.96"`) resolved via `rustup update stable` → 1.97.0.

### 2.1–2.4 Execution and results

Discovery found all direct Rust dependencies already at their latest crates.io-published versions satisfying existing semver ranges — no Rust major-version bumps were available. Only 4 transitive deps had compatible bumps: `cc`, `rand`, `thread_local`, `tinyvec` (Tier 1, one `cargo update` commit). npm: `react`/`react-dom`/`@types/react` bumped (Tier 2). `vocs` 1.x → 2.x was attempted and reverted — vocs 2.x rewrote its build pipeline onto a React Server Components framework (`waku`), requiring cascading peer deps (`vite@^8`, `waku@^1.0.0-beta.6`) and still failing on an internal `unplugin-icons` virtual-module resolution error; this is a genuine architectural migration exceeding the ~1 hour localized-fix threshold, not a mechanical bump. `vocs` remains pinned at `^1.0.0-alpha.62` (resolving to 1.4.1). `wrangler` (4.65.0 → 4.110.0 available) intentionally left untouched — owned by Stream C's scope now that Workers Caching requires ≥4.69.0 (noted in `crates/imgx-edge/wrangler.toml`).

Final status: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green; `docs:build` green.

---

## 3. Stream B: Cloudflare Images URL & Parameter Parity

### 3.a URL-shape decision — RESOLVED

**Owner selected Option B: migrate the primary scheme.** imgx's canonical URL format is now `GET /cdn-cgi/image/<OPTIONS>/<SOURCE-IMAGE>` — fixed prefix, options-first — matching Cloudflare's exact convention. This is a breaking change: the old trailing-options shape (`/<image-path>/<transforms>`) is retired outright, not kept as a fallback. See `crates/imgx/src/router.rs` and `docs/INVARIANTS.md` INV-5 for the implementation and the specific relaxation decision (a segment after the prefix with no `=` is treated as a transform-less image path rather than rejected, since Cloudflare's own "at least one parameter" rule would otherwise break imgx's passthrough use case).

### 3.b Parameter gap table (Phase B3+ — not yet implemented)

Priority key — **P0**: blocks any honest "1:1 parity" claim. **P1**: parameter accepted by Cloudflare that imgx rejects or handles differently. **P2**: net-new feature.

| # | Gap | Cloudflare behavior | imgx today | Priority |
|---|---|---|---|---|
| 1 | URL shape | options-first, fixed prefix | **DONE** — matches exactly | — |
| 2 | Arbitrary source URL | any absolute http(s) URL | **DONE** — behind `IMGX_ALLOW_REMOTE_SOURCES` (default off), SSRF-safe fetcher — see `docs/CLOUDFLARE_PARITY.md` gap 2 | — |
| 3 | `fit` vocabulary | 8 values incl. `scale-down` (default), `crop`, `aspect-crop`, `scale-up` | 6 values, `contain` default | P0 |
| 4 | `quality`/`q` | 1–100 + perceptual strings; default 85 | 1–100 only; default 80 | P1 |
| 5 | `format`/`f` | adds `baseline-jpeg`, `json` | neither | P1 |
| 6 | `compression=fast` | quality-for-speed tradeoff | **DONE** — see `docs/CLOUDFLARE_PARITY.md` gap 6 | — |
| 7 | `onerror=redirect` | redirect on failure | raw-bytes fallback (INVARIANTS.md) | P1 |
| 8 | `slow-connection-quality`/`scq` | client-hint quality override | **DONE** — see `docs/CLOUDFLARE_PARITY.md` gap 8 | — |
| 9 | `trim` | border-color-aware, per-side | numeric threshold only | P1 |
| 10 | `border` | draws border | **DONE** (spec-derived URL syntax — Cloudflare's own `border` is Workers-only, no URL form published) — see `docs/CLOUDFLARE_PARITY.md` gap 10 | — |
| 11 | `draw` (overlays) | watermark/overlay array | **DONE** — parsing, compositing, and remote overlay fetch (behind `IMGX_ALLOW_DRAW_OVERLAYS`, default off) all shipped — see `docs/CLOUDFLARE_PARITY.md` gap 11 | — |
| 12 | `gravity`/`g` | named + `auto` + possible focal-point coords | compass words + `smart`/`attention` | P0 (verify) |
| 13 | `rotate` ordering | before resize/crop | unverified | P0 (verify) |

**Update — all 13 gaps now closed or deliberately scoped.** See `docs/CLOUDFLARE_PARITY.md` for the authoritative, row-by-row status with verified-vs-spec-derived-vs-gated provenance for each. Summary: gaps 1, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13 are implemented and tested. Gap 2 (arbitrary remote-URL sources) and the remote-fetch half of gap 11 (`draw` overlay images) were implemented together as a single SSRF-safe fetcher (`IMGX_ALLOW_REMOTE_SOURCES` / `IMGX_ALLOW_DRAW_OVERLAYS`, both default off) — see `docs/INVARIANTS.md` for the SSRF-boundary invariant this added.

### 3.c–3.e See original phase breakdown

Phase B1 (docs honesty fix) shipped as part of the URL migration — `docs/pages/migrating-from-cloudflare.mdx` and `docs/pages/transforms.mdx` now describe the real, matching URL shape rather than overclaiming compatibility. Phase B2 (URL shape) shipped. Phases B3–B5 (parameter parity, `docs/CLOUDFLARE_PARITY.md` tracker) shipped in full per the update above.

---

## 4. Stream C: Cloudflare Deployment Architecture — RESOLVED (build it)

**Owner approved building the workers-rs edge layer.** `crates/imgx-edge` is a minimal Worker (pure pass-through reverse proxy, Workers Caching via `wrangler.toml`'s `[cache]` block) in front of the real imgx origin. Discovery found `.github/workflows/docs.yml` already deploys the docs site to Cloudflare Pages under project name `zimgx` (not `imgx`) via `wrangler pages deploy docs/dist --project-name=zimgx` — confirming `imgx-edge`'s new `wrangler.toml` (colocated in `crates/imgx-edge/`, not repo root) is a fully separate deployable with no collision risk. No `routes`/zone binding is configured yet — which hostname this Worker fronts remains an open decision (see `docs/pages/cloudflare-edge-deployment.mdx`).

**Non-goals confirmed:** no libvips/transform logic in the Worker (hard WASM-sandbox constraint), no Cache API (Workers Caching chosen instead — per-datacenter-local semantics would be a correctness trap), no speculative KV/R2/D1/Durable Object bindings.

---

## 5. Sequencing — executed in parallel

**Owner selected: run in parallel.** All three streams executed concurrently in isolated git worktrees, merged back sequentially with verification (fmt/clippy/test) after each merge.

---

## 6. Open Questions — final disposition

- **OQ-2 — RESOLVED: implemented, off by default.** Arbitrary remote-URL main-image sources are supported behind `IMGX_ALLOW_REMOTE_SOURCES` (default `false`, preserving every existing deployment's behavior unchanged). When enabled, fetches go through a dedicated SSRF-safe fetcher: scheme allowlist (http/https only), DNS-resolution-time rejection of private/loopback/link-local/CGNAT ranges (checked against the *resolved* IP, not just the hostname string, so DNS-rebinding can't bypass it) plus a direct check for literal-IP-address hosts (found during implementation: hyper's connector skips DNS resolution entirely when the host is already an IP literal, e.g. a URL pointed straight at `169.254.169.254`, which would otherwise silently bypass the resolver-based guard), a capped and re-validated redirect chain, and the same streaming size/timeout caps as the existing origin fetcher (INV-12). See `docs/INVARIANTS.md` for the added SSRF-boundary invariant and `docs/CLOUDFLARE_PARITY.md` gap 2 for the full guard list and test provenance.
- **OQ-3 — RESOLVED: keep imgx's existing defaults.** `fit` stays `contain`-default and `quality` stays `80`-default; Cloudflare's `scale-down`/`85` remain available as explicit non-default values. Rationale: changing a default is an observable behavior change for every existing imgx deployment that relies on the current default, which is a materially different (and much larger-blast-radius) decision than adding a new opt-in value — not something to fold into a parity pass silently. If a future need arises to match Cloudflare's defaults exactly, it should be its own conscious, documented decision (mirroring how `docs/INVARIANTS.md` treats INV-1's cache-key-preservation policy), not a byproduct of this PRD.
- **OQ-4 — RESOLVED: additive, non-breaking.** `onerror=redirect` ships as an explicit opt-in per request; imgx's default (raw-bytes fallback on transform failure, INV-13) is unchanged. See gap 7 in `docs/CLOUDFLARE_PARITY.md`.
- **OQ-5 — RESOLVED: both syntaxes coexist.** Legacy numeric `trim=<threshold>` (border-color-aware, unchanged) and Cloudflare's per-side `trim.top`/`.right`/`.bottom`/`.left` (pixel or 0–1 fraction, independent semantics) both work, documented as two distinct features rather than one replacing the other. See gap 9.
- **OQ-9 — RESOLVED: in scope, shipped.** `draw` overlay parsing and the full libvips compositing pipeline are implemented and tested against local image buffers; the remote-fetch half was completed together with OQ-2 (gated separately behind `IMGX_ALLOW_DRAW_OVERLAYS`, also default off). See gap 11.
- **OQ-11 — RESOLVED (verified against current Cloudflare docs).** `ctx.cache.purge()` propagates globally via Cloudflare's Instant Purge infrastructure — not a per-datacenter-only operation — so no global-consistency assumption is being made incorrectly. Two things worth remembering for `crates/imgx-edge` if purge logic is ever added: purges are scoped to the Worker + calling entrypoint (a Worker can't purge another Worker's cache, and no zone-level purge touches Workers Caching), and by default the deployed Worker *version* is part of the cache key, so every deploy starts cold unless `cache.cross_version_cache` is explicitly enabled in `wrangler.toml`.
- **OQ-12 — NOT RESOLVABLE FROM THIS REPO: requires the account owner's real infrastructure intent.** Whether `imgx.pages.dev`/the `zimgx` project (now migrated to Workers static assets, see the root `wrangler.toml` and `.github/workflows/docs.yml`) should ever also serve the image-proxy edge is a product/DNS decision that depends on domains and traffic plans this repo's contents don't capture. Recommendation, not a decision: keep it docs-only — `crates/imgx-edge` is deliberately a separate deployable specifically so the two can evolve independently (see `docs/pages/cloudflare-edge-deployment.mdx`).
- **OQ-14 — NOT RESOLVABLE FROM THIS REPO: no real domain/zone is available to this session.** `crates/imgx-edge/wrangler.toml`'s `routes` block is intentionally left as a placeholder/comment rather than guessing a hostname (see the file itself). This is a genuine operator input (which domain, which zone in their Cloudflare account) — fabricating one would be worse than leaving it explicit.
- **OQ-15 — RESOLVED: CI-owned, manual-dispatch until OQ-14 is set.** Added `.github/workflows/deploy-edge.yml`, mirroring `docs.yml`'s pattern (checkout, `cargo install worker-build`, `wrangler deploy` via `cloudflare/wrangler-action@v4`) but triggered by `workflow_dispatch` only (not on every push) since deploying `imgx-edge` before `IMGX_ORIGIN_URL` and a real route are configured would deploy a non-functional Worker. Once OQ-14 is answered and the wrangler.toml placeholders are filled in, switching the trigger to `push`-on-`crates/imgx-edge/**` is a one-line change.
