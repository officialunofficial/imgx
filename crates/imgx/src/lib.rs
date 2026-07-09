//! imgx — image proxy and transform server. Module tree mirrors zimgx's
//! src/*.zig 1:1 for reviewability; see docs/INVARIANTS.md for the
//! behaviors that must survive the port.

pub mod config;
pub mod http;
pub mod transform;
