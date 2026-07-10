// Hand-declared libvips/glib C FFI surface. Signatures verified against the
// headers in /opt/homebrew/Cellar/vips/8.18.0_2/include/vips/{image,header,
// foreign,resample,error,vips}.h. Kept minimal — extend only as the pipeline
// needs a new C call, mirroring the surface enumerated in the Zig
// implementation's src/vips/bindings.zig.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use libc::{c_char, c_double, c_int, c_void, size_t};

#[repr(C)]
pub struct VipsImage {
    _private: [u8; 0],
}

#[repr(C)]
pub struct VipsArrayDouble {
    _private: [u8; 0],
}

#[repr(C)]
pub struct VipsArea {
    _private: [u8; 0],
}

pub type GQuark = u32;

unsafe extern "C" {
    // -- lifecycle --
    pub fn vips_init(argv0: *const c_char) -> c_int;
    pub fn vips_shutdown();
    pub fn g_object_unref(object: *mut c_void);
    pub fn g_free(mem: *mut c_void);

    // -- errors --
    pub fn vips_error_buffer() -> *const c_char;
    pub fn vips_error_clear();

    // -- load --
    // G_GNUC_NULL_TERMINATED varargs option list, e.g.
    // vips_image_new_from_buffer(buf, len, "", NULL) or
    // vips_image_new_from_buffer(buf, len, "", "n", -1, NULL).
    pub fn vips_image_new_from_buffer(
        buf: *const c_void,
        len: size_t,
        option_string: *const c_char,
        ...
    ) -> *mut VipsImage;

    // -- header / metadata --
    pub fn vips_image_get_width(image: *const VipsImage) -> c_int;
    pub fn vips_image_get_height(image: *const VipsImage) -> c_int;
    pub fn vips_image_get_bands(image: *const VipsImage) -> c_int;
    pub fn vips_image_hasalpha(image: *mut VipsImage) -> c_int;
    pub fn vips_image_get_int(
        image: *const VipsImage,
        name: *const c_char,
        out: *mut c_int,
    ) -> c_int;
    pub fn vips_image_set_int(image: *mut VipsImage, name: *const c_char, i: c_int);

    // -- resample --
    pub fn vips_thumbnail_image(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        width: c_int,
        ...
    ) -> c_int;

    // -- geometry --
    pub fn vips_extract_area(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        left: c_int,
        top: c_int,
        width: c_int,
        height: c_int,
        ...
    ) -> c_int;
    pub fn vips_rot(in_: *mut VipsImage, out: *mut *mut VipsImage, angle: c_int, ...) -> c_int;
    pub fn vips_flip(in_: *mut VipsImage, out: *mut *mut VipsImage, direction: c_int, ...)
    -> c_int;
    pub fn vips_find_trim(
        in_: *mut VipsImage,
        left: *mut c_int,
        top: *mut c_int,
        width: *mut c_int,
        height: *mut c_int,
        ...
    ) -> c_int;
    pub fn vips_arrayjoin(
        in_: *mut *mut VipsImage,
        out: *mut *mut VipsImage,
        n: c_int,
        ...
    ) -> c_int;
    pub fn vips_embed(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        x: c_int,
        y: c_int,
        width: c_int,
        height: c_int,
        ...
    ) -> c_int;
    pub fn vips_flatten(in_: *mut VipsImage, out: *mut *mut VipsImage, ...) -> c_int;

    // -- color / effects --
    pub fn vips_sharpen(in_: *mut VipsImage, out: *mut *mut VipsImage, ...) -> c_int;
    pub fn vips_gaussblur(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        sigma: c_double,
        ...
    ) -> c_int;
    pub fn vips_linear1(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        a: c_double,
        b: c_double,
        ...
    ) -> c_int;
    pub fn vips_gamma(in_: *mut VipsImage, out: *mut *mut VipsImage, ...) -> c_int;
    pub fn vips_colourspace(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        space: c_int,
        ...
    ) -> c_int;
    pub fn vips_extract_band(
        in_: *mut VipsImage,
        out: *mut *mut VipsImage,
        band: c_int,
        ...
    ) -> c_int;
    pub fn vips_bandjoin2(
        in1: *mut VipsImage,
        in2: *mut VipsImage,
        out: *mut *mut VipsImage,
        ...
    ) -> c_int;

    // -- save --
    pub fn vips_jpegsave_buffer(
        in_: *mut VipsImage,
        buf: *mut *mut c_void,
        len: *mut size_t,
        ...
    ) -> c_int;
    pub fn vips_pngsave_buffer(
        in_: *mut VipsImage,
        buf: *mut *mut c_void,
        len: *mut size_t,
        ...
    ) -> c_int;
    pub fn vips_webpsave_buffer(
        in_: *mut VipsImage,
        buf: *mut *mut c_void,
        len: *mut size_t,
        ...
    ) -> c_int;
    pub fn vips_heifsave_buffer(
        in_: *mut VipsImage,
        buf: *mut *mut c_void,
        len: *mut size_t,
        ...
    ) -> c_int;
    pub fn vips_gifsave_buffer(
        in_: *mut VipsImage,
        buf: *mut *mut c_void,
        len: *mut size_t,
        ...
    ) -> c_int;

    // -- background color arrays (for flatten/embed) --
    pub fn vips_array_double_new(array: *const c_double, n: c_int) -> *mut VipsArrayDouble;
    pub fn vips_area_unref(area: *mut VipsArea);
}

// VipsAngle (resample.h / conversion enums)
pub const VIPS_ANGLE_D0: c_int = 0;
pub const VIPS_ANGLE_D90: c_int = 1;
pub const VIPS_ANGLE_D180: c_int = 2;
pub const VIPS_ANGLE_D270: c_int = 3;

// VipsDirection
pub const VIPS_DIRECTION_HORIZONTAL: c_int = 0;
pub const VIPS_DIRECTION_VERTICAL: c_int = 1;

// VipsSize
pub const VIPS_SIZE_BOTH: c_int = 0;
pub const VIPS_SIZE_UP: c_int = 1;
pub const VIPS_SIZE_DOWN: c_int = 2;
pub const VIPS_SIZE_FORCE: c_int = 3;

// VipsInteresting
pub const VIPS_INTERESTING_NONE: c_int = 0;
pub const VIPS_INTERESTING_CENTRE: c_int = 1;
pub const VIPS_INTERESTING_ENTROPY: c_int = 2;
pub const VIPS_INTERESTING_ATTENTION: c_int = 3;
pub const VIPS_INTERESTING_LOW: c_int = 4;
pub const VIPS_INTERESTING_HIGH: c_int = 5;
pub const VIPS_INTERESTING_ALL: c_int = 6;

// VipsInterpretation (subset used by the pipeline)
pub const VIPS_INTERPRETATION_sRGB: c_int = 22;
pub const VIPS_INTERPRETATION_LCH: c_int = 19;

// VipsExtend
pub const VIPS_EXTEND_BLACK: c_int = 0;
pub const VIPS_EXTEND_COPY: c_int = 1;
pub const VIPS_EXTEND_REPEAT: c_int = 2;
pub const VIPS_EXTEND_MIRROR: c_int = 3;
pub const VIPS_EXTEND_WHITE: c_int = 4;
pub const VIPS_EXTEND_BACKGROUND: c_int = 5;
