# Contributing to imgx

## Setup

Requires Rust (stable) and libvips 8.14+ (`brew install vips` on macOS, `apt-get install libvips-dev` on Debian/Ubuntu).

```sh
cargo build --workspace
cargo test --workspace
```

See `CLAUDE.md` for the full workspace layout, code conventions, and patterns this codebase follows.

## Before opening a PR

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three run in CI and are required to pass.

## Commit messages

This repo uses [Conventional Commits](https://www.conventionalcommits.org/) (`feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `perf`, `ci`, optionally scoped e.g. `fix(cache): ...`). `CHANGELOG.md` is generated from these via `cliff.toml`/`release-plz` — commit messages become the changelog, so make them accurate.

## Tests

- New behavior needs a test in the same file, `#[cfg(test)] mod tests` at the bottom (see `CLAUDE.md`'s Code Conventions).
- If you're changing behavior that has a documented invariant in `docs/INVARIANTS.md`, update that doc alongside the code — it's the spec the test suite is checked against, not just a description.
- Prefer a real test over a broadened `#[allow(...)]` or a skipped assertion. If a test would require infrastructure that doesn't exist yet (e.g. a new fixture format), add it rather than testing around the gap.

## FFI / `unsafe` changes

Any new libvips C call goes in `crates/imgx-vips`, never directly in `crates/imgx` (which is `#![forbid(unsafe_code)]`). Verify C function signatures and enum values against the actual installed libvips headers — don't guess them from memory or from another project's bindings. An omitted option isn't always a no-op: `save_avif()` once omitted the `compression` option to `vips_heifsave_buffer`, which made libvips silently default to HEVC instead of AV1 — every "AVIF" response was either failing outright or (on a runtime with an HEVC encoder) silently mislabeling HEVC-in-a-HEIF-container as `image/avif`. See `docs/INVARIANTS.md` INV-13 for the downstream cache-poisoning consequence that incident triggered.
