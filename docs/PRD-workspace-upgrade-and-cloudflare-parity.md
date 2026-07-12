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
| 2 | Arbitrary source URL | any absolute http(s) URL | Only configured-origin paths | P0 (deferred — SSRF surface, needs its own design pass) |
| 3 | `fit` vocabulary | 8 values incl. `scale-down` (default), `crop`, `aspect-crop`, `scale-up` | 6 values, `contain` default | P0 |
| 4 | `quality`/`q` | 1–100 + perceptual strings; default 85 | 1–100 only; default 80 | P1 |
| 5 | `format`/`f` | adds `baseline-jpeg`, `json` | neither | P1 |
| 6 | `compression=fast` | quality-for-speed tradeoff | none | P2 |
| 7 | `onerror=redirect` | redirect on failure | raw-bytes fallback (INVARIANTS.md) | P1 |
| 8 | `slow-connection-quality`/`scq` | client-hint quality override | none | P2 |
| 9 | `trim` | border-color-aware, per-side | numeric threshold only | P1 |
| 10 | `border` | draws border | none | P2 |
| 11 | `draw` (overlays) | watermark/overlay array | nothing | P2 (largest net-new) |
| 12 | `gravity`/`g` | named + `auto` + possible focal-point coords | compass words + `smart`/`attention` | P0 (verify) |
| 13 | `rotate` ordering | before resize/crop | unverified | P0 (verify) |

These gaps are **not yet implemented** — Phase B3/B4 (parameter parity) and Phase B0 (Cloudflare-docs verification for gaps 4, 5, 12) remain as follow-up work.

### 3.c–3.e See original phase breakdown

Phase B1 (docs honesty fix) shipped as part of the URL migration — `docs/pages/migrating-from-cloudflare.mdx` and `docs/pages/transforms.mdx` now describe the real, matching URL shape rather than overclaiming compatibility. Phase B2 (URL shape) shipped. Phases B3–B5 (parameter parity, new `docs/CLOUDFLARE_PARITY.md` tracker) remain open.

---

## 4. Stream C: Cloudflare Deployment Architecture — RESOLVED (build it)

**Owner approved building the workers-rs edge layer.** `crates/imgx-edge` is a minimal Worker (pure pass-through reverse proxy, Workers Caching via `wrangler.toml`'s `[cache]` block) in front of the real imgx origin. Discovery found `.github/workflows/docs.yml` already deploys the docs site to Cloudflare Pages under project name `zimgx` (not `imgx`) via `wrangler pages deploy docs/dist --project-name=zimgx` — confirming `imgx-edge`'s new `wrangler.toml` (colocated in `crates/imgx-edge/`, not repo root) is a fully separate deployable with no collision risk. No `routes`/zone binding is configured yet — which hostname this Worker fronts remains an open decision (see `docs/pages/cloudflare-edge-deployment.mdx`).

**Non-goals confirmed:** no libvips/transform logic in the Worker (hard WASM-sandbox constraint), no Cache API (Workers Caching chosen instead — per-datacenter-local semantics would be a correctness trap), no speculative KV/R2/D1/Durable Object bindings.

---

## 5. Sequencing — executed in parallel

**Owner selected: run in parallel.** All three streams executed concurrently in isolated git worktrees, merged back sequentially with verification (fmt/clippy/test) after each merge.

---

## 6. Remaining Open Questions

- **OQ-2:** Arbitrary remote-URL sources (Stream B gap 2) — security posture, config surface, env-var names. Not yet designed.
- **OQ-3:** Adopt Cloudflare's defaults (`fit=scale-down`, `quality=85`) globally, per-route, or not at all?
- **OQ-4:** Error-fallback reconciliation — `onerror=redirect` vs imgx's raw-bytes-on-failure invariant.
- **OQ-5:** `trim` semantics — replace, or keep legacy numeric alongside Cloudflare's dotted per-side keys?
- **OQ-9:** Is `draw` (overlays) in scope for this effort, or its own follow-up PRD?
- **OQ-11:** Confirm Workers Caching purge/propagation semantics against current Cloudflare docs before relying on global-consistency assumptions.
- **OQ-12:** Is `imgx.pages.dev` (served via the `zimgx` Pages project) intended to ever host the image-proxy edge, or is it docs-only permanently?
- **OQ-14:** Which hostname/zone should `crates/imgx-edge` actually front?
- **OQ-15:** Should this repo own Worker deploy via CI (a new `.github/workflows/` entry), or remain manual/dashboard-driven consistent with today's `zimgx` Pages deploy?
