# imgx

Fast, single-binary image proxy and transformation server written in Rust, using libvips.

## Build & Test

```bash
cargo build                                              # debug build
cargo build --release -p imgx                            # optimized build
cargo build --profile profiling -p imgx                  # release opts + debug symbols, for samply
cargo test --workspace                                   # run all unit/integration tests
cargo fmt --all                                           # format
cargo fmt --all -- --check                                # format check (CI enforced)
cargo clippy --workspace --all-targets -- -D warnings     # lint (CI enforced, see clippy.toml)
```

Requires libvips and glib headers. On macOS: `brew install vips`. On Alpine: `apk add vips-dev musl-dev pkgconfig` (see `.cargo/config.toml` for the musl `crt-static` workaround needed for dynamic libvips linking).

## Workspace layout

Two crates:
- `crates/imgx-vips/` — hand-rolled libvips FFI (the crate's `unsafe`/audit boundary). Raw `extern "C"` declarations in `ffi.rs`, a safe RAII wrapper in `image.rs`.
- `crates/imgx/` — the binary. `#![forbid(unsafe_code)]` — all `unsafe` stays quarantined in `imgx-vips`.

Module tree inside `crates/imgx/src/` mirrors the domain: `config`, `router`, `server`, `http/{errors,response}`, `cache/{mod,memory,noop,r2,tiered}`, `origin/{source,fetcher,r2}`, `s3/client`, `transform/{params,negotiate,pipeline}`.

See `docs/INVARIANTS.md` for behaviors that must never change without a conscious, documented decision.

## Commit Convention

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>
```

**Types:** `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `ci`

**Scopes** (optional, match source modules): `transform`, `cache`, `origin`, `vips`, `server`, `config`, `http`, `s3`, `router`

Examples:
```
feat(transform): add saturation parameter
fix(cache): prevent eviction race on concurrent put
refactor(server): extract route handlers into separate functions
perf(vips): reduce buffer copies in pipeline execution
test(config): cover R2 validation edge cases
ci: add clippy lint step
docs: update transform parameter reference
chore: bump minimum Rust version
```

`cliff.toml` generates `CHANGELOG.md` from these commits — keep messages accurate, they become the changelog.

## Code Conventions

### File Structure

Each module file follows this layout:

1. **Module doc comment** — `//!` block at the top describing the module's purpose
2. **Imports** — `std`, then external crates, then `crate::`/`super::` imports
3. **Error types** — `#[derive(Debug, Error)] pub enum FooError { ... }` (thiserror)
4. **Types / structs / enums** — public API types
5. **Public functions / impls** — module-level public API
6. **Private helpers** — internal functions
7. **Tests** — `#[cfg(test)] mod tests { ... }` block at the end, in the same file (not a separate test file)

### Naming

- `snake_case` for functions, variables, modules, fields
- `PascalCase` for types (structs, enums, traits)
- `SCREAMING_SNAKE_CASE` for constants
- Environment variables use `IMGX_` prefix (legacy `ZIMGX_` read as a documented fallback — see `config.rs`)

### Style

- Doc comments (`///`) on public types and functions; plain `//` for internal explanations, omitted when code is self-evident
- `thiserror` for error enums, not `anyhow` — these are structured domain errors the caller matches on and maps to HTTP status codes, not opaque application errors
- Prefer explicit `match` over `.unwrap()`-heavy chains; `?` for propagation
- Test names are lowercase descriptive phrases as function names: `fn parse_empty_string_returns_default_params()`
- Every behavior ported from the original Zig implementation should have a test ported alongside it — see `docs/INVARIANTS.md` for the list of behaviors this applies to most strictly

### Patterns

- **Cache trait**: `Cache` in `cache/mod.rs` uses `impl Future<Output = ...> + Send` return-position-impl-trait (RPITIT) async methods, not `dyn Cache`. The backend set is closed (Memory/Noop/R2/Tiered), so `TieredCache<L1, L2>` is generic over its backends and the server picks a concrete `AppCache` enum variant at startup — no boxing, no object-safety cost.
- **Config loading**: env vars with `IMGX_` prefix (falling back to `ZIMGX_`), struct defaults via `Default`, validation in a separate `validate()` pass returning the first invalid field.
- **Parsing**: return typed errors with specific variants (e.g. `ParseError::InvalidWidth`), not generic strings.
- **`parse_str` / `as_str`**: enum string conversion via explicit `match`, not derived/automatic — named `parse_str` (not `from_str`) to avoid colliding with `std::str::FromStr` (clippy `should_implement_trait`).
- **FFI isolation**: any new libvips C call goes in `imgx-vips`, never directly in `imgx`. Verify C signatures against the installed libvips headers, don't guess them.

### What to Avoid

- Don't add `unsafe` to the `imgx` crate — it's `forbid`den; new FFI needs go in `imgx-vips`.
- Don't reach for `dyn Trait` when the implementer set is closed — prefer an enum or generics (see the Cache trait pattern above).
- Don't add comments that restate what the code does.
- Don't port Zig-specific workarounds whose underlying problem Rust's ownership model already solves (e.g. manual arena allocators, "last-fetched buffer" ownership hacks) — see `docs/INVARIANTS.md`'s "Assumptions & non-invariants" section for the specific list.
