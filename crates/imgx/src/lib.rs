//! imgx — image proxy and transform server. Module tree mirrors zimgx's
//! src/*.zig 1:1 for reviewability; see docs/INVARIANTS.md for the
//! behaviors that must survive the port.
//!
//! All `unsafe` is quarantined in the `imgx-vips` crate (the FFI/audit
//! boundary) — this crate forbids it entirely.
#![forbid(unsafe_code)]

pub mod cache;
pub mod config;
pub mod http;
pub mod origin;
pub mod s3;
pub mod transform;
