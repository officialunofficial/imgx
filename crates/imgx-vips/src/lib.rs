//! Hand-rolled libvips FFI bindings and a safe RAII wrapper, scoped to
//! exactly the C surface the imgx transform pipeline needs. Mirrors
//! zimgx's src/vips/bindings.zig.

mod error;
mod ffi;
mod image;

pub use error::VipsError;
pub use image::{init, shutdown, VipsImage};
