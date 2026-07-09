//! Hand-rolled libvips FFI bindings and a safe RAII wrapper, scoped to
//! exactly the C surface the imgx transform pipeline needs. Mirrors
//! zimgx's src/vips/bindings.zig.

mod error;
mod ffi;
mod image;

pub use error::VipsError;
pub use image::{arrayjoin_vertical, init, shutdown, ThumbnailOptions, VipsImage};

/// libvips C enum constants needed to call the FFI-wrapped operations
/// (angle, direction, size, interesting/crop mode, colorspace, extend).
pub mod consts {
    pub use crate::ffi::{
        VIPS_INTERPRETATION_sRGB, VIPS_ANGLE_D0, VIPS_ANGLE_D180, VIPS_ANGLE_D270, VIPS_ANGLE_D90,
        VIPS_DIRECTION_HORIZONTAL, VIPS_DIRECTION_VERTICAL, VIPS_EXTEND_BACKGROUND,
        VIPS_INTERESTING_ATTENTION, VIPS_INTERESTING_CENTRE, VIPS_INTERESTING_ENTROPY,
        VIPS_INTERPRETATION_LCH, VIPS_SIZE_DOWN, VIPS_SIZE_FORCE, VIPS_SIZE_UP,
    };
}
