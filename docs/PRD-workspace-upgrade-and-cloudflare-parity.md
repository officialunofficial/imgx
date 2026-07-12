# PRD: imgx — Workspace Dependency Upgrade & Cloudflare Images Parity

**Status:** Draft — awaiting human sign-off on open questions (Section 6)
**Repo:** `officialunofficial/imgx` (Cargo workspace + vocs docs site)
**Author:** Fable 5 (planning). **Executor:** Sonnet 5. **Reviewer:** repo owner.

---

## 1. Executive Summary

Three work streams. **Stream A** brings every Rust crate dependency (workspace root + `crates/imgx` + `crates/imgx-vips`) and every npm devDependency (vocs docs site) to latest safe versions, keeping fmt/clippy/tests/docs-build/Docker green throughout, with patch/minor bumps batched and each major bump as an isolated commit+test cycle. **Stream B** closes the gap between imgx's URL/transform surface and Cloudflare Images' real `/cdn-cgi/image/<OPTIONS>/<SOURCE>` convention — starting with a URL-shape decision that requires explicit human sign-off, then a prioritized parameter-parity plan across 13 identified gaps, and mandatory corrections to `docs/pages/migrating-from-cloudflare.mdx`, which currently overclaims compatibility. **Stream C** proposes a `workers-rs` edge Worker (wrangler.toml, Workers Caching) in front of the real imgx origin as a cache/router layer — explicitly NOT a port of the transform pipeline, since libvips cannot run inside a Worker's WASM sandbox. Stream A should land first to establish a clean baseline; Streams B and C have no hard dependency on it and can run on parallel branches if speed is preferred.

---

## 2. Stream A: Full Workspace Dependency Upgrade

### 2.0 Pre-flight blocker check (do this before anything else)

- **Toolchain mismatch:** local toolchain is cargo/rustc **1.94.1**, but the workspace declares `rust-version = "1.96"` in root `Cargo.toml`. Resolve before any upgrade work:
  - Try `rustup update stable` (or install 1.96+ via rustup). If the environment cannot get ≥1.96, **STOP and flag** — either the environment must be fixed or `rust-version` must be discussed with the owner (do not silently lower it).
  - Verify with `rustc --version` and a clean `cargo build`.
- Confirm a green baseline **before touching anything**, so upgrade failures are attributable:
  ```
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  npm ci && npm run docs:build
  docker build .   # (locate Dockerfile; see Phase A1)
  ```
  Record results. If baseline is already red, fix or flag before proceeding.

### 2.1 Phase A1 — Discovery (no changes committed)

**Goal:** exact current-vs-latest table for every dependency, with breaking-change notes for majors.

Rust (no `cargo-outdated` installed in this environment):
1. Preferred: `cargo install cargo-outdated` then `cargo outdated --workspace --root-deps-only`. If install fails (network/proxy), fall back to:
   - `cargo update --dry-run` (shows compatible-range bumps), plus
   - per-crate latest lookup via `cargo search <crate> --limit 1` or `cargo info <crate>` for majors.
2. Produce a table (in the PR description, not a committed .md) with: crate, current, latest compatible, latest major, risk tier (below).
3. For each candidate **major** bump, read the crate's CHANGELOG/release notes for the versions being crossed. Crates flagged for extra scrutiny:
   - **axum 0.8 →** newer: router/extractor API churn is common between axum 0.x majors; check `Router`, extractor trait, and middleware layering changes.
   - **tower 0.5 / tower-http 0.7 →** newer: feature-flag renames and `ServiceBuilder` layer signature changes; imgx uses `util`/`limit`/`load-shed` (tower) and `trace` (tower-http).
   - **reqwest 0.13 →** newer: imgx pins `default-features = false` + rustls; verify the rustls feature name hasn't changed and no default TLS backend sneaks in.
   - **metrics 0.24.6 / metrics-exporter-prometheus 0.18.3:** these two must stay version-compatible with each other (the exporter pins a `metrics` version). **Preserve `default-features = false` on `metrics-exporter-prometheus` on any upgrade** — it is deliberate, to avoid its built-in scrape server since `/metrics` is served through the app's own axum router. Check that the feature set needed for a manually-rendered Prometheus handle still exists under the new feature flags.
   - Also check: `lru 0.18` (API churn history), `rusty-s3 0.10` (pre-1.0, signing API changes), `tikv-jemallocator 0.7`, `xxhash-rust 0.8`, `wiremock 0.6.5`, `jiff 0.2` (dev-dep, pre-1.0 → 0.x majors likely).
   - `crates/imgx-vips`: `libc 0.2`, `thiserror 2`, build-dep `pkg-config 0.3` — expect compatible-range only; any change here touches the unsafe/FFI boundary, so run the full test suite even for "trivial" bumps.

npm:
1. `npm outdated` at repo root.
2. **vocs** (currently `^1.0.0-alpha.62`, pre-1.0 alpha): check its current published version (`npm view vocs versions` / changelog on GitHub). Determine whether a newer alpha/beta/stable exists and whether the config schema (`vocs.config.ts`), MDX page frontmatter, or sidebar format changed between alpha.62 and target. **Alpha-to-alpha bumps can break arbitrarily** — treat any vocs bump as a "major" (own commit, full `docs:build` + `docs:preview` visual spot-check).
3. React 19.x / @types/react — expect minor bumps only.
4. **wrangler ^4.65.0:** before bumping, locate the deploy config — check for `wrangler.toml`/`wrangler.jsonc` and any Cloudflare Pages/Workers deploy wiring (CI workflow files under `.github/workflows/`). If wrangler is used for docs deploy, verify the deploy command still works. If no wrangler config exists in-repo (true as of this PRD), flag it — wrangler may be vestigial or deploy config lives outside the repo (see Stream C, OQ-7).

Docker:
1. Locate the multi-stage Dockerfile (Alpine + `apk add vips-dev musl-dev pkgconfig`, musl `crt-static` workaround in `.cargo/config.toml`).
2. Note the Rust base image tag — if it pins a Rust version below the workspace `rust-version` (or below what new deps require), bump it in the same PR.
3. **The `.cargo/config.toml` musl `crt-static` workaround must survive untouched** unless a toolchain bump specifically obsoletes it (if so, document why and verify the dynamic libvips link still works in the Alpine image).

### 2.2 Phase A2 — Risk-tiered upgrade execution

| Tier | What | Batch strategy | Commit convention |
|---|---|---|---|
| 1 | All patch/minor (semver-compatible) Rust bumps | Single `cargo update` batch, one commit | `chore: update compatible Rust dependencies` |
| 2 | All patch/minor npm bumps (react, @types, wrangler minor) | Single batch, one commit | `chore: update npm devDependencies` |
| 3 | Each Rust **major** bump | One crate (or coupled pair, e.g. metrics + exporter; tower + tower-http) per commit, full check cycle between | `chore(<scope>): bump <crate> X → Y` |
| 4 | vocs bump (treat as major regardless of version delta) | Own commit, `docs:build` + visual spot-check of rendered pages | `chore: bump vocs to <version>` |
| 5 | Docker base-image / toolchain bumps if needed | Own commit, full `docker build` verification | `ci: bump Docker Rust base image` |

Ordering within Tier 3: least-coupled first (lru, xxhash-rust, jiff, wiremock, rusty-s3), then the HTTP stack as scrutiny-heavy finale (reqwest → tower/tower-http → axum → metrics pair).

**Full check cycle** (run after every Tier 3/4/5 commit, and once after each of Tier 1/2):
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run docs:build          # after any npm change
docker build                 # after Tier 5, and once at the end regardless
```

### 2.3 Rollback criteria (per bump)

| Failure mode | Action |
|---|---|
| Compile error with an obvious 1:1 API rename | **Fix forward** — mechanical migration, same commit |
| Compile error requiring architectural change | **Fix forward if <~1 hour of localized change**; otherwise revert, keep the crate at current major, flag as "deferred — needs dedicated migration" |
| Any **test failure** from genuine dependency behavior change | **Revert and flag.** Behavior changes in an image proxy (caching, hashing, HTTP semantics) violate `docs/INVARIANTS.md` discipline — a human decides |
| Clippy `-D warnings` failure from new lints | Fix forward (targeted `#[allow]` with comment only for true false positives) |
| `docs:build` failure after vocs bump | Attempt migration per vocs changelog; if undocumented, **revert vocs and flag** |
| Docker build failure (musl link, vips-dev, crt-static) | **Revert the offending bump and flag** |
| `cargo test` flake (passes on retry, unrelated to bump) | Note it, proceed; don't attribute to the bump without a second failure |

Every reverted/deferred bump goes into the PR description under "Deferred upgrades" with the failing output excerpt.

### 2.4 Stream A acceptance criteria

- [ ] Toolchain ≥ workspace `rust-version` confirmed (or blocker escalated and resolved).
- [ ] Every dependency at latest compatible, and latest major unless explicitly deferred with a documented reason.
- [ ] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green on the final commit.
- [ ] `npm run docs:build` succeeds; docs render correctly on visual spot-check if vocs was bumped.
- [ ] `docker build` succeeds end-to-end (Alpine/musl/libvips dynamic link intact; `.cargo/config.toml` workaround preserved or its removal justified).
- [ ] `metrics-exporter-prometheus` still has `default-features = false` and `/metrics` still serves through the app router.
- [ ] Commit history follows Conventional Commits.
- [ ] Deferred-upgrades list (possibly empty) delivered in the PR description.

---

## 3. Stream B: Cloudflare Images URL & Parameter Parity

### 3.a The URL-shape decision (BLOCKING — requires owner sign-off before implementation)

Cloudflare's URL-transform product:
```
https://<ZONE>/cdn-cgi/image/<OPTIONS>/<SOURCE-IMAGE>
```
Fixed `/cdn-cgi/image/` prefix; comma-separated `key=value` options **first**; source is an absolute origin path **or any absolute http(s) URL**.

imgx today (`crates/imgx/src/router.rs::resolve`, INV-5 in `docs/INVARIANTS.md`):
```
GET /<image-path>/<transform-string>
```
No prefix; transforms **last** (final `/` segment containing `=`); source always resolved against the configured origin via `IMGX_ORIGIN_PATH_PREFIX` — arbitrary absolute-URL sources are not supported.

Three real options:

| | Option A: Additive dual-route | Option B: Migrate primary scheme | Option C: Semantics-only parity |
|---|---|---|---|
| **What** | Add `/cdn-cgi/image/<options>/<source>` alongside existing route | Replace imgx's scheme with Cloudflare's options-first + prefix | Keep URL shape as-is; make parameter names/values/semantics match 1:1; fix docs to stop claiming shape compatibility |
| **Breaking?** | No — existing URLs keep working | **Yes** — every deployed imgx URL in the wild breaks | No |
| **Router complexity** | Second parse path keyed on the fixed prefix; prefix check is cheap and unambiguous | Single parse path, but a migration/compat story is needed anyway (so really Option A + a deprecation) | None |
| **INV-5 impact** | Unchanged for the legacy route; new invariant added for the prefix route | Rewritten — a conscious, documented invariant change | None |
| **Cache-key impact** | Must decide whether `/cdn-cgi/image/w=100/a.jpg` and `/a.jpg/w=100` normalize to the **same** cache key (they should). Requires canonicalizing params before hashing | Same normalization question, plus existing cache entries keyed on old URLs are orphaned | None |
| **Migration value** | Cloudflare-shaped URLs point at imgx unchanged — true "drop-in" claim becomes honest | Same, but at existing users' expense | "Drop-in" claim must be retracted; migration requires URL rewriting on the user's side |
| **Arbitrary-URL source** | Enables it on the new route only (gated, see Gap 2) | Enables it | Not addressed |

**Recommendation: Option A** (additive dual-route). It makes the "drop-in replacement for Cloudflare Images" positioning honest without breaking a single existing deployment. Option B buys nothing Option A doesn't, at the cost of every existing user. Option C is the fallback if route-count/cache-normalization complexity isn't worth it — but then the docs correction (3.c Phase B1) becomes even more important because the compatibility claim must be fully retracted.

**Do not begin Stream B implementation until the owner picks A, B, or C.** (Open Question OQ-1.)

### 3.b Parameter gap table

Priority key — **P0**: blocks any honest "1:1 parity" claim (shape or core semantics). **P1**: parameter accepted by Cloudflare that imgx rejects or handles differently. **P2**: net-new feature; absence is documentable, not a parity lie.

| # | Gap | Cloudflare behavior | imgx today | Priority | Notes for implementation |
|---|---|---|---|---|---|
| 1 | URL shape | `/cdn-cgi/image/<opts>/<src>`, options-first | options-last, no prefix | **P0** | Resolved by OQ-1 decision; touches `router.rs`, INV-5 |
| 2 | Arbitrary source URL | Source may be any absolute http(s) URL | Only configured-origin paths | **P0** (if Option A/B) | **SSRF surface.** Off by default behind `IMGX_ALLOW_REMOTE_SOURCES` + host allowlist. OQ-2 |
| 3 | `fit` vocabulary | 8 values: `scale-down` (default), `contain`, `cover`, `crop`, `aspect-crop`, `pad`, `squeeze`, `scale-up` | 6 values: `contain` (default), `cover`, `fill`, `inside`, `outside`, `pad` | **P0** | Default differs: `scale-down` never upscales. Accept CF names as aliases only where semantics truly match — verify `fill`≈`squeeze` and `outside`-vs-`scale-up` with pixel fixtures before aliasing. Default-change is OQ-3 |
| 4 | `quality`/`q` | 1–100 **and** `high`/`medium-high`/`medium-low`/`low`; default 85 | 1–100 only; default 80 | **P1** | Map perceptual strings to documented integer equivalents (fetch Cloudflare's mapping during execution). Default-change (80→85) is OQ-3 |
| 5 | `format`/`f` | adds `baseline-jpeg`, `json` (metadata-only response) | neither | **P1** | `baseline-jpeg` likely needs a new FFI flag in `imgx-vips`. `json` is a new response type through `http/response.rs` — define schema from a real Cloudflare response during execution |
| 6 | `compression=fast` | quality-for-speed encode tradeoff | none | **P2** | Map to libvips encoder effort/speed settings per format |
| 7 | `onerror=redirect` | Redirect to original on transform failure (same-zone only) | Serves raw origin bytes on encode failure (INVARIANTS.md) | **P1** | Proposal: accept `onerror=redirect` as opt-in per-request; keep raw-bytes as imgx default. Reconciling is OQ-4 |
| 8 | `slow-connection-quality`/`scq` | Quality override from client hints (`rtt`, `save-data`, `ect`, `downlink`) | none | **P2** | Needs header parsing + cache-key participation (same URL, different quality by hint → must vary cache key) |
| 9 | `trim` | Border-color-aware; per-side `trim.top`/`trim.left`/etc. | Single numeric threshold | **P1** | Different semantics, not a rename. Maps to libvips `find_trim` + per-side crop. OQ-5 on legacy alias |
| 10 | `border` | Draws border (color + width/per-side) around output | none | **P2** | libvips embed/extend |
| 11 | `draw` (overlays) | Array of overlays: `url`, `width`/`height` (px or 0–1 fraction), `repeat`, `top`/`left`/`bottom`/`right`, `opacity`, `background`, `rotate`, reuses `fit`/`gravity` | nothing | **P2** (largest net-new) | Requires overlay fetching (same SSRF gating as Gap 2), compositing FFI in `imgx-vips`, array-valued param syntax. Ship last; possibly its own follow-up PRD (OQ-9) |
| 12 | `gravity`/`g` | Named directions, `auto` (saliency), possibly focal-point coords — **not yet verified** | compass words + `smart`/`attention` | **P0 (verify)** | Execution must fetch Cloudflare's gravity docs first and diff exactly |
| 13 | `rotate` ordering | Applied **before** resize/crop; width/height refer to post-rotation axes | unverified | **P0 (verify)** | Read `crates/imgx/src/transform/pipeline.rs`; add a fixture test with a non-square rotated image where wrong ordering changes output dimensions |

Also during execution: sweep the full Cloudflare options list for any parameter not in this seed list (`anim`, `background`, `blur`, `brightness`, `contrast`, `dpr`, `gamma`, `metadata`, `sharpen`, etc.) and confirm imgx's existing handling matches names, ranges, and defaults.

### 3.c Implementation phases (file-mapped)

**Phase B0 — Verification pass (no code changes).**
- Fetch and pin (quote in PR description) current Cloudflare docs for: gravity (Gap 12), quality string mappings (Gap 4), `format=json` schema (Gap 5), full options list sweep.
- Read `transform/pipeline.rs` and document actual operation order vs Cloudflare's documented rotate-before-resize (Gap 13).
- Read `router.rs::resolve` and INV-5 to spec the exact parse change for the chosen URL option.
- Output: finalized gap table with the two "verify" rows resolved, delivered for owner review alongside OQ answers.

**Phase B1 — Docs honesty fix (ships regardless of everything else, can ship first).**
- `docs/pages/migrating-from-cloudflare.mdx`: remove/correct the "compatible with Cloudflare's path structure" claim. State precisely that imgx's trailing-options form resembles the *hosted* `imagedelivery.net` convention, not the `/cdn-cgi/image/` URL-transform convention. Document the current real migration path until/unless the OQ-1 route ships.
- `docs/pages/transforms.mdx`: add a per-parameter Cloudflare-compatibility note where names/defaults/values differ today.
- New `docs/CLOUDFLARE_PARITY.md` (separate from the existing Zig-parity `docs/PARITY.md`): the gap table from 3.b as a living checklist, status per row, borrowing the byte-for-byte/pixel-fixture methodology from `docs/PARITY.md`.

**Phase B2 — URL shape (gated on OQ-1).** Assuming Option A:
- `router.rs`: second parse path — if path starts with `/cdn-cgi/image/`, split next segment as options, remainder as source (path or absolute URL). Legacy path untouched.
- Cache-key normalization: canonicalize parsed params (sorted, defaults elided) before the xxh3 cache key so both URL shapes for the same transform share one cache entry.
- `docs/INVARIANTS.md`: document the new route invariant; INV-5 unchanged.
- Arbitrary-URL source (Gap 2): new config in `config.rs` (`IMGX_ALLOW_REMOTE_SOURCES` bool + `IMGX_REMOTE_SOURCE_ALLOWLIST`, default off), new fetch path in `origin/`, SSRF guards (deny non-http(s) schemes, deny private/link-local IP ranges after DNS resolution, cap redirects and response size).

**Phase B3 — P0/P1 parameter parity.**
- `transform/params.rs`: `fit` new values + aliases, quality strings, `format` new values, `onerror`, Cloudflare-style `trim`, gravity additions per B0 findings. Typed `ParseError` variants, `parse_str`/`as_str` via explicit match.
- `transform/pipeline.rs`: new fit modes' resize/crop math; rotate ordering fix if B0 found a mismatch; trim execution.
- `crates/imgx-vips/`: new libvips calls (baseline JPEG flag, `find_trim`, later composite) — FFI stays here, signatures verified against installed headers.
- `http/response.rs` / `http/errors.rs`: `format=json` metadata response; `onerror=redirect` 302 path.

**Phase B4 — P2 features (each independently shippable, in this order).**
1. `compression=fast` — encoder settings only.
2. `border` — pipeline + small FFI addition.
3. `scq` client-hints — header parsing + cache-key variance.
4. `draw` overlays — largest; param array syntax, overlay fetch (reuses Gap 2 gating), composite FFI, pipeline integration. Acceptable to split into its own follow-up PR/PRD (OQ-9).

**Phase B5 — Docs finalization.** Update `transforms.mdx`, `migrating-from-cloudflare.mdx`, `docs/CLOUDFLARE_PARITY.md` to reflect what shipped. `npm run docs:build` green.

### 3.d Explicit non-goals (state in `docs/CLOUDFLARE_PARITY.md`, do not silently omit)

| Out of scope | Why |
|---|---|
| Workers `fetch(url, {cf: {image: {...}}})` parity | Options passed as a JS object on a Workers subrequest — no URL convention to match, and imgx has no Workers runtime for this specific surface |
| Images binding (`env.IMAGES.input(...).transform(...).output(...)`) | Programmatic chainable stream API tied to the Workers platform; imgx is an HTTP proxy, not an embeddable JS API |
| Dashboard Transformation Flows (Provider flows for Fastly-param rewriting; Custom flows pairing conditions with parameter actions) | Account-dashboard **automation rules** — a control-plane feature. imgx is a single-binary self-hosted proxy with no dashboard; no URL shape to port. **Stretch goal (not this PRD):** server-side default-transform config and/or documented reverse-proxy rewrite recipes — propose separately if wanted |
| `imagedelivery.net` hosted upload/storage model (account hash, image IDs, named variants) | imgx is deliberately unmanaged/BYO-storage per its own docs; no upload API or image registry. Named-variant presets could be a future config feature but are not URL parity |

### 3.e Test & verification plan

- **Parser tests**: every new parameter value, alias, error variant, and both URL shapes producing identical normalized params for identical transforms. Names as lowercase descriptive phrases per convention.
- **Cache-key tests**: same source + params via legacy route and `/cdn-cgi/image/` route hash to the same key; `scq`-effective quality varies the key (Gap 8).
- **Pixel-fixture tests** (methodology of existing `docs/PARITY.md`): every fit alias claim, rotate-then-resize ordering, trim, border, and eventually draw. Where a real Cloudflare zone is available, capture reference outputs; otherwise encode documented semantics as dimension/pixel assertions, noted as "spec-derived, not CF-captured."
- **SSRF tests** (Gap 2): remote sources rejected by default; allowlist honored; private-IP targets refused; wiremock-based integration test.
- **Invariant tests**: existing INV-5 tests untouched and green; new-route invariant tests added and referenced from `docs/INVARIANTS.md`.
- Full gates on every Stream B commit: `cargo fmt --all -- --check`, clippy `-D warnings`, `cargo test --workspace`.

---

## 4. Stream C: Cloudflare Deployment Architecture (wrangler.toml, workers-rs, Workers Cache, imgx.pages.dev)

### 4.0 Hard constraint — read before proposing anything

**libvips cannot run inside a Cloudflare Worker. Full stop.** `crates/imgx` links libvips dynamically via C FFI (`unsafe extern "C"` in `crates/imgx-vips`, pkg-config, Alpine/musl at runtime). Workers execute Rust via `workers-rs` compiled to `wasm32-unknown-unknown` inside a V8 isolate/WASM sandbox: no dynamic C library loading, no filesystem, no arbitrary syscalls. This is an architectural incompatibility, not a style preference. The real `imgx` binary keeps running where it runs today; Workers can only ever be a layer *in front of* it.

### 4.1 Current-state discovery (must run before any design work)

- No `wrangler.toml`/`wrangler.jsonc` exists in the repo. `wrangler ^4.65.0` is a devDependency shared with `vocs` docs tooling but has no committed config.
- `.github/` contains only `dependabot.yml` — no Pages/Workers deploy workflow.
- The user states `imgx.pages.dev` is live today. **This cannot currently be explained by anything in the repo.** Before designing Stream C, confirm directly with the owner (do not guess):
  - Dashboard Git integration, manual `wrangler pages deploy`, or stale/unmaintained?
  - What does `imgx.pages.dev` actually serve today — the `vocs` docs site only, or something else?

This discovery blocks every phase below.

### 4.2 Recommended architecture

A `workers-rs` Worker sitting **in front of** the real imgx origin, acting purely as a thin edge layer:

- Recognizes image-transform requests (existing route today; the `/cdn-cgi/image/` route from Stream B if it ships).
- On cache hit: serves directly from Workers Cache **without invoking the origin**.
- On cache miss: forwards unmodified to the real imgx server, passes through `Cache-Control`/`ETag` so Workers Cache stores correctly.
- Does **not** parse transform parameters, does **not** call libvips, does **not** reimplement any part of `transform/pipeline.rs`.

This is additive infrastructure, needing the same explicit owner sign-off as Stream B's URL-shape decision (OQ-10).

### 4.3 Workers Caching vs Cache API

| | Workers Caching (`[cache]` in wrangler.toml) | Cache API (`caches.default`) |
|---|---|---|
| Invocation on hit | Worker **not invoked** — served from `Cache-Control` per RFC 9111 | Worker **always** executes; must call `cache.match()` explicitly |
| Scope | Read-through, request collapsing, tiered caching | No collapsing, no tiering |
| Geographic scope | Effectively global-consistent via tiering | **Local to the data center handling the request only** |
| Config surface | `[cache] enabled = true`, or per-export `[exports.<Name>.cache]` in `wrangler.toml` | Programmatic in Worker code |
| Invalidation | `ctx.cache.purge()` | Manual `cache.delete()` per PoP (doesn't purge other PoPs) |
| Cloudflare's guidance | "For new Workers, prefer Workers Caching" | Reserved for fine-grained programmatic control |

**Recommendation: Workers Caching**, not Cache API. imgx already implements its own explicit multi-tier cache (`cache/{memory,r2,tiered}`); duplicating that via Cache API would mean re-deriving keys/TTLs/invalidation in a second place, and Cache API's per-PoP-local, non-shared semantics are a correctness trap if anything assumes global visibility. Workers Caching's read-through model composes cleanly: imgx already decides what's cacheable via its own `Cache-Control` headers; the Worker just has to not get in the way. Confirm purge/propagation semantics against current Cloudflare docs at implementation time before relying on any global-consistency assumption (OQ-11).

### 4.4 Interaction with Stream B

If Stream B ships the `/cdn-cgi/image/` route, the Worker is the natural place to own the exact prefix match at the edge and `Cache-Control`/`ETag` passthrough discipline. If Stream B unifies cache keys across both URL shapes server-side, the Worker's edge cache must not accidentally re-split them.

### 4.5 Relationship to `imgx.pages.dev` and the existing "plain CDN" option

Three things must not be conflated:

1. **`imgx.pages.dev`** — presumed docs-site-only pending discovery (4.1). Do not assume it becomes the image-proxy edge (OQ-12).
2. **Plain CDN in front of imgx** — already documented in `migrating-from-cloudflare.mdx`, zero repo changes required, works today.
3. **New `workers-rs` edge Worker** — the Stream C proposal; a separate Cloudflare deployable, independent of the Pages project serving docs.

Whether Stream C is built at all is a decision, not a foregone conclusion (OQ-13).

### 4.6 File targets

- **New** `wrangler.toml` at repo root, only after 4.1 confirms no collision with `imgx.pages.dev`'s existing deploy. `compatibility_date` pinned recent; `[cache] enabled = true` (or per-export); explicit `routes`/zone binding (OQ-14 — which hostname?); no speculative KV/R2/D1/Durable Object bindings (the design in 4.2 needs none).
- **New crate** `crates/imgx-edge` (workers-rs project, built with `worker-build`, not plain `cargo build`) — confirm during implementation whether it joins the root workspace `members` for fmt/clippy consistency.
- `crates/imgx-edge/src/lib.rs` — routing/forwarding only, no transform logic.
- `docs/pages/migrating-from-cloudflare.mdx` — document the new option alongside (not overwriting) existing plain-CDN guidance.
- CI: new GitHub Actions workflow for `wrangler deploy`, only if the repo should own that deploy (OQ-15).

### 4.7 Phases

- **C0 — Discovery** (4.1, blocking). Output: factual writeup, no code.
- **C1 — Decision gate.** Owner signs off on: build at all (OQ-13), Workers Caching vs Cache API (resolved — Workers Caching), target hostname/zone (OQ-14), relationship to `imgx.pages.dev` (OQ-12).
- **C2 — Scaffold.** Minimal `crates/imgx-edge`: builds, deploys, forwards all traffic untouched, no caching yet. Verify `worker-build`/`wrangler deploy --dry-run` against a test zone.
- **C3 — Caching.** Add `[cache]` config, verify `Cache-Control` passthrough (observable via `cf-cache-status`), confirm imgx's own headers are already cache-correct (verify, don't assume).
- **C4 — Stream B integration** (if applicable). Add `/cdn-cgi/image/` prefix match once OQ-1 lands.
- **C5 — CI/deploy automation.** Only if OQ-15 resolves toward repo-owned deploy.

### 4.8 Acceptance criteria

- [ ] Phase C0 discovery writeup delivered and reviewed before any `wrangler.toml`/crate is created.
- [ ] `wrangler.toml` explicit about zone/hostname; does not silently overwrite `imgx.pages.dev`'s existing config.
- [ ] `crates/imgx-edge` contains no transform/pipeline/libvips logic — routing and caching only.
- [ ] Workers Caching selected over Cache API, with the local-only-per-PoP gotcha documented as the explicit rejection reason.
- [ ] Cache hit path measurably bypasses the origin (`cf-cache-status: HIT`, no corresponding origin request).
- [ ] `Cache-Control`/`ETag` headers from imgx pass through unmodified.
- [ ] `migrating-from-cloudflare.mdx` presents three distinct options (plain CDN / this Worker / neither) without conflating them.
- [ ] If Stream B's `/cdn-cgi/image/` route ships, the Worker's prefix match and imgx's own route agree byte-for-byte on what counts as a transform request.

### 4.9 Non-goals

| Out of scope | Why |
|---|---|
| Running libvips or any part of `transform/pipeline.rs` inside a Worker | Hard technical impossibility (4.0) |
| Reimplementing transform logic in Rust-to-WASM from scratch, bypassing libvips | Multi-month project orthogonal to this PRD's goal (edge caching) |
| Migrating `crates/imgx` itself off Docker/Alpine | Out of scope; Stream C only adds a layer in front of the existing deployment target |
| Cache API as the caching mechanism | Deliberately rejected per 4.3 |
| Speculative KV/R2/D1/Durable Object bindings | The design needs none |
| Making `imgx.pages.dev` the image-proxy edge without confirmation | Assumed docs-site-only until Phase C0 confirms otherwise |

---

## 5. Sequencing Recommendation

**Stream A first, then Streams B/C — recommended.** Stream A is lower-risk and mechanical, establishing a clean dependency baseline so new code in B/C (axum routing, reqwest fetching for remote sources) is written once against upgraded APIs.

Streams B and C have **no hard dependency** on Stream A. Phase B0 (verification) and B1 (docs honesty fix) can start immediately on a parallel branch — B1 should ship early regardless of sequencing, since the current docs overclaim is live today. Stream C's Phase C0 (discovery) is similarly independent and can start immediately. Code phases beyond that risk a rebase across an axum/tower major bump; only parallelize with owner buy-in.

---

## 6. Deliverables Summary

| Deliverable | Stream | Files |
|---|---|---|
| Upgraded Cargo workspace + lockfile | A | root + `crates/*/Cargo.toml`, `Cargo.lock` |
| Upgraded npm deps | A | `package.json`, `package-lock.json` |
| Docker build re-verified (bumped if needed) | A | `Dockerfile`, possibly `.cargo/config.toml` (only with justification) |
| Deferred-upgrades report | A | PR description |
| Docs correction | B1 | `docs/pages/migrating-from-cloudflare.mdx`, `docs/pages/transforms.mdx` |
| Cloudflare parity tracker | B1 | `docs/CLOUDFLARE_PARITY.md` (new) |
| URL route work (per OQ-1) | B2 | `router.rs`, cache-key sites, `config.rs`, `origin/`, `docs/INVARIANTS.md` |
| Parameter parity | B3/B4 | `transform/{params,pipeline}.rs`, `http/{response,errors}.rs`, `imgx-vips/src/{ffi,image}.rs` |
| Tests per 3.e | B | inline `mod tests` blocks, fixtures |
| Discovery writeup | C0 | none (report only) |
| Edge Worker + config | C2+ | `wrangler.toml`, `crates/imgx-edge/` |
| Deploy CI (if OQ-15) | C5 | `.github/workflows/` |

---

## 7. Open Questions — require explicit human sign-off before execution

- **OQ-1 — RESOLVED (owner, 2026-07-12): Option B, migrate primary scheme.** imgx's primary URL scheme moves to Cloudflare's options-first + `/cdn-cgi/image/` prefix convention. This is a breaking change for any existing imgx deployment's URLs — call this out prominently in the CHANGELOG and consider a migration note / major version bump. The old `/<path>/<transforms>` trailing-options route is retired as primary; decide during B2 implementation whether to keep it briefly as a deprecated fallback (logged warning) or remove outright in the same change — default to removing outright unless the owner asks for a deprecation window, since Option B was explicitly chosen over the non-breaking Option A.
- **OQ-2:** Arbitrary remote-URL sources — security posture, config surface, env-var names.
- **OQ-3:** Adopt Cloudflare's defaults (`fit=scale-down`, `quality=85`) globally, per-route, or not at all?
- **OQ-4:** Error-fallback reconciliation — `onerror=redirect` vs imgx's raw-bytes-on-failure invariant.
- **OQ-5:** `trim` semantics — replace, or keep legacy numeric alongside Cloudflare's dotted per-side keys?
- **OQ-6 (Stream A):** Toolchain <1.96 blocker — fix environment or discuss lowering `rust-version`?
- **OQ-7 (Stream A):** No wrangler config found in-repo — is wrangler vestigial, or does deploy config live elsewhere?
- **OQ-8 — RESOLVED (owner, 2026-07-12): run in parallel.** Stream A (dependency upgrade) and Streams B/C (Cloudflare work) proceed on parallel work simultaneously rather than strictly sequenced. Execution uses isolated git worktrees per stream to avoid clobbering shared files (root `Cargo.toml`, `package.json`), merged back deliberately rather than via concurrent commits to one working tree.
- **OQ-9:** Is `draw` (overlays) in scope for this effort, or its own follow-up PRD?
- **OQ-10 — RESOLVED (owner, 2026-07-12): yes, build the workers-rs edge layer.**
- **OQ-11:** Confirm Workers Caching purge/propagation semantics against current Cloudflare docs before relying on global-consistency assumptions.
- **OQ-12:** Is `imgx.pages.dev` the docs site only (assumed), or intended to become the image-proxy edge?
- **OQ-13 — RESOLVED (owner, 2026-07-12): superseded by OQ-10 (yes, build it).**
- **OQ-14:** If Stream C proceeds, which hostname/zone does the new Worker actually front?
- **OQ-15:** Should this repo own Worker deploy via CI, or remain manual/dashboard-driven?
