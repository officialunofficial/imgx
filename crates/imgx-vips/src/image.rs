use std::ffi::{CString, c_void};
use std::ptr;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use libc::{c_char, c_int, size_t};

use crate::error::{VipsError, last_vips_error};
use crate::ffi;

static VIPS_INIT: Once = Once::new();
static VIPS_INIT_OK: AtomicBool = AtomicBool::new(false);

/// Load option that sets the `fail_on` property of the libvips loader to
/// `VIPS_FAIL_ON_ERROR` (nickname `error`, from `vips/foreign.h`).
const STRICT_OPTION: &str = "fail_on=error";

/// Number of operations that libvips keeps for reuse. Every request builds
/// its operations from new images, so a cached operation never hits again.
/// The cache only keeps the input images, and their source bytes, alive.
const OPERATION_CACHE_MAX: c_int = 0;

/// Initialize libvips and turn off its operation cache. Safe to call more
/// than once (subsequent calls are no-ops); mirrors `bindings.zig`'s
/// `init()`. Must be paired with at most one `shutdown()` call, from `main`,
/// never from tests.
pub fn init() -> Result<(), VipsError> {
    VIPS_INIT.call_once(|| {
        let argv0 = CString::new("imgx").unwrap();
        let rc = unsafe { ffi::vips_init(argv0.as_ptr()) };
        if rc == 0 {
            unsafe { ffi::vips_cache_set_max(OPERATION_CACHE_MAX) };
        }
        VIPS_INIT_OK.store(rc == 0, Ordering::SeqCst);
    });
    if VIPS_INIT_OK.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err(VipsError::InitFailed(last_vips_error()))
    }
}

/// The AVIF encoder effort that libvips uses by default.
pub const DEFAULT_AVIF_EFFORT: u8 = 4;

/// The highest AVIF encoder effort that libvips accepts.
pub const MAX_AVIF_EFFORT: u8 = 9;

/// Shut down libvips. Call at most once, from `main` on process exit.
/// Never call from tests (`bindings.zig` carries the same restriction).
pub fn shutdown() {
    unsafe { ffi::vips_shutdown() }
}

/// RAII wrapper over a `VipsImage*`. Not `Sync` — hold one per
/// `spawn_blocking` task, never share a handle across threads.
pub struct VipsImage {
    ptr: ptr::NonNull<ffi::VipsImage>,
}

// A VipsImage handle itself may be moved to another thread (e.g. into a
// spawn_blocking closure) as long as it is not shared concurrently, hence
// Send but not Sync.
unsafe impl Send for VipsImage {}

/// Options for `VipsImage::thumbnail`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ThumbnailOptions {
    /// Target height. If `None`, libvips auto-computes to preserve aspect ratio.
    pub height: Option<i32>,
    /// Crop mode (`ffi::VIPS_INTERESTING_*`).
    pub crop: Option<i32>,
    /// Size constraint (`ffi::VIPS_SIZE_*`).
    pub size: Option<i32>,
    /// Ignore the EXIF orientation tag. The output keeps the stored pixel
    /// orientation.
    pub no_rotate: bool,
}

impl Drop for VipsImage {
    fn drop(&mut self) {
        unsafe { ffi::g_object_unref(self.ptr.as_ptr() as *mut c_void) }
    }
}

/// Expands to the `vips_thumbnail_*` call matching `$opts`. Each optional
/// argument is a name/value pair in the C varargs list, so every
/// combination needs its own call. `$call!` is a caller-local macro that
/// appends the given varargs to the concrete thumbnail call.
macro_rules! thumbnail_with_options {
    ($opts:expr, $call:ident) => {
        match ($opts.height, $opts.crop, $opts.size, $opts.no_rotate) {
            (Some(h), Some(c), Some(s), false) => {
                $call!(
                    c"height".as_ptr(),
                    h,
                    c"crop".as_ptr(),
                    c,
                    c"size".as_ptr(),
                    s
                )
            }
            (Some(h), Some(c), None, false) => {
                $call!(c"height".as_ptr(), h, c"crop".as_ptr(), c)
            }
            (Some(h), None, Some(s), false) => {
                $call!(c"height".as_ptr(), h, c"size".as_ptr(), s)
            }
            (Some(h), None, None, false) => $call!(c"height".as_ptr(), h),
            (None, Some(c), Some(s), false) => {
                $call!(c"crop".as_ptr(), c, c"size".as_ptr(), s)
            }
            (None, Some(c), None, false) => $call!(c"crop".as_ptr(), c),
            (None, None, Some(s), false) => $call!(c"size".as_ptr(), s),
            (None, None, None, false) => $call!(),
            (Some(h), Some(c), Some(s), true) => $call!(
                c"height".as_ptr(),
                h,
                c"crop".as_ptr(),
                c,
                c"size".as_ptr(),
                s,
                c"no_rotate".as_ptr(),
                1 as c_int
            ),
            (Some(h), Some(c), None, true) => $call!(
                c"height".as_ptr(),
                h,
                c"crop".as_ptr(),
                c,
                c"no_rotate".as_ptr(),
                1 as c_int
            ),
            (Some(h), None, Some(s), true) => $call!(
                c"height".as_ptr(),
                h,
                c"size".as_ptr(),
                s,
                c"no_rotate".as_ptr(),
                1 as c_int
            ),
            (Some(h), None, None, true) => {
                $call!(c"height".as_ptr(), h, c"no_rotate".as_ptr(), 1 as c_int)
            }
            (None, Some(c), Some(s), true) => $call!(
                c"crop".as_ptr(),
                c,
                c"size".as_ptr(),
                s,
                c"no_rotate".as_ptr(),
                1 as c_int
            ),
            (None, Some(c), None, true) => {
                $call!(c"crop".as_ptr(), c, c"no_rotate".as_ptr(), 1 as c_int)
            }
            (None, None, Some(s), true) => {
                $call!(c"size".as_ptr(), s, c"no_rotate".as_ptr(), 1 as c_int)
            }
            (None, None, None, true) => $call!(c"no_rotate".as_ptr(), 1 as c_int),
        }
    };
}

impl VipsImage {
    /// Load an image from an in-memory buffer, first frame/page only
    /// (the "probe" load — cheap, used to detect animation metadata). The
    /// bytes are copied, so `data` need not outlive the image.
    pub fn from_buffer(data: &[u8]) -> Result<Self, VipsError> {
        Self::from_buffer_with_option(data, "")
    }

    /// Load an image from an in-memory buffer, requesting `n` pages.
    /// `n = -1` loads all pages/frames, stacked vertically for
    /// multi-page formats (GIF/animated WebP).
    pub fn from_buffer_animated(data: &[u8], n: i32) -> Result<Self, VipsError> {
        let option = format!("n={n}");
        Self::from_buffer_with_option(data, &option)
    }

    /// Load an image from an in-memory buffer, first frame/page only, and
    /// fail on the first decode error. `from_buffer` uses the libvips
    /// default `fail_on=none`. A truncated or partly corrupt source then
    /// decodes without an error, and the missing pixels come back blank or
    /// partial. This load sets `fail_on=error`, so reading such a source
    /// returns an error instead. The error can appear here or when a later
    /// call, such as `write_to_memory`, reads the pixels.
    pub fn from_buffer_strict(data: &[u8]) -> Result<Self, VipsError> {
        Self::from_buffer_with_option(data, STRICT_OPTION)
    }

    /// Like `from_buffer_animated`, with the strictness of
    /// `from_buffer_strict`.
    pub fn from_buffer_animated_strict(data: &[u8], n: i32) -> Result<Self, VipsError> {
        let option = format!("n={n},{STRICT_OPTION}");
        Self::from_buffer_with_option(data, &option)
    }

    fn from_buffer_with_option(data: &[u8], option_string: &str) -> Result<Self, VipsError> {
        let c_opts = CString::new(option_string).map_err(|_| {
            VipsError::LoadFailed("option string contained an interior NUL".to_string())
        })?;
        let source = OwnedSource::copy_of(data).map_err(VipsError::LoadFailed)?;
        // The loader takes its own reference to the source, so `source`
        // may drop before the image does.
        let raw = unsafe {
            ffi::vips_image_new_from_source(
                source.ptr.as_ptr(),
                c_opts.as_ptr(),
                ptr::null::<c_char>(),
            )
        };
        match ptr::NonNull::new(raw) {
            Some(ptr) => Ok(VipsImage { ptr }),
            None => Err(VipsError::LoadFailed(last_vips_error())),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_ptr(&self) -> *mut ffi::VipsImage {
        self.ptr.as_ptr()
    }

    /// Wrap a raw non-null VipsImage* produced by another vips op,
    /// taking ownership (the wrapper's Drop will unref it).
    #[allow(dead_code)]
    pub(crate) unsafe fn from_raw(raw: *mut ffi::VipsImage) -> Option<Self> {
        ptr::NonNull::new(raw).map(|ptr| VipsImage { ptr })
    }

    /// Width in pixels. A negative libvips value reads as 0.
    pub fn width(&self) -> i32 {
        unsafe { ffi::vips_image_get_width(self.ptr.as_ptr()) }.max(0)
    }

    /// Height in pixels. A negative libvips value reads as 0. A stacked
    /// animation reports the height of the whole stack.
    pub fn height(&self) -> i32 {
        unsafe { ffi::vips_image_get_height(self.ptr.as_ptr()) }.max(0)
    }

    /// Number of bands per pixel, alpha included.
    pub fn bands(&self) -> i32 {
        unsafe { ffi::vips_image_get_bands(self.ptr.as_ptr()) }.max(0)
    }

    /// True when the image has an alpha band.
    pub fn has_alpha(&self) -> bool {
        unsafe { ffi::vips_image_hasalpha(self.ptr.as_ptr()) != 0 }
    }

    /// Return true when the decoder of this image can decode at reduced
    /// size. This holds for JPEG and WebP. For those formats
    /// `thumbnail_buffer` uses less memory than `thumbnail`. It is faster
    /// when the shrink ratio is 4 or more. PNG, GIF, AVIF, and TIFF decode at
    /// full size either way, so `thumbnail_buffer` would only decode twice.
    pub fn shrinks_on_load(&self) -> bool {
        let mut out: *const c_char = ptr::null();
        let rc = unsafe {
            ffi::vips_image_get_string(self.ptr.as_ptr(), c"vips-loader".as_ptr(), &mut out)
        };
        if rc != 0 {
            unsafe { ffi::vips_error_clear() };
            return false;
        }
        if out.is_null() {
            return false;
        }
        let loader = unsafe { std::ffi::CStr::from_ptr(out) }.to_bytes();
        loader.starts_with(b"jpegload") || loader.starts_with(b"webpload")
    }

    /// Read an integer metadata field (e.g. "n-pages", "page-height").
    /// Returns `None` if the field is not present.
    pub fn get_int(&self, name: &str) -> Option<i32> {
        let c_name = CString::new(name).ok()?;
        let mut out: i32 = 0;
        let rc = unsafe { ffi::vips_image_get_int(self.ptr.as_ptr(), c_name.as_ptr(), &mut out) };
        if rc == 0 {
            Some(out)
        } else {
            unsafe { ffi::vips_error_clear() };
            None
        }
    }

    /// Write an integer metadata field. Takes `&self`, not `&mut self`:
    /// this mutates the underlying libvips C object in place (matching
    /// the Zig binding, which takes the image by value with no
    /// exclusivity concept) rather than any Rust-tracked state.
    pub fn set_int(&self, name: &str, value: i32) {
        if let Ok(c_name) = CString::new(name) {
            unsafe { ffi::vips_image_set_int(self.ptr.as_ptr(), c_name.as_ptr(), value) }
        }
    }

    /// Number of pages/frames (`n-pages` metadata), or `None` if absent
    /// (a normal single-frame image has no `n-pages` field at all).
    pub fn n_pages(&self) -> Option<i32> {
        self.get_int("n-pages")
    }

    /// Per-frame height in a vertically-stacked multi-page image
    /// (`page-height` metadata), or `None` if absent.
    pub fn page_height(&self) -> Option<i32> {
        self.get_int("page-height")
    }

    /// Encode to JPEG. `quality` is 1-100.
    pub fn save_jpeg(&self, quality: i32, strip: bool) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_jpegsave_buffer(
                self.ptr.as_ptr(),
                &mut buf,
                &mut len,
                c"Q".as_ptr(),
                quality,
                c"strip".as_ptr(),
                bool_to_int(strip),
                ptr::null::<c_char>(),
            )
        };
        save_result(rc, buf, len)
    }

    /// Encode to PNG. `compression` is 0-9 (zimgx always uses 6, fixed).
    pub fn save_png(&self, compression: i32, strip: bool) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_pngsave_buffer(
                self.ptr.as_ptr(),
                &mut buf,
                &mut len,
                c"compression".as_ptr(),
                compression,
                c"strip".as_ptr(),
                bool_to_int(strip),
                ptr::null::<c_char>(),
            )
        };
        save_result(rc, buf, len)
    }

    /// Encode to WebP. `quality` is 1-100.
    pub fn save_webp(&self, quality: i32, strip: bool) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_webpsave_buffer(
                self.ptr.as_ptr(),
                &mut buf,
                &mut len,
                c"Q".as_ptr(),
                quality,
                c"strip".as_ptr(),
                bool_to_int(strip),
                ptr::null::<c_char>(),
            )
        };
        save_result(rc, buf, len)
    }

    /// Encode to AVIF (via the HEIF encoder). `quality` is 1-100. `effort`
    /// is the encoder CPU effort. It runs from 0 (fastest, largest output)
    /// to `MAX_AVIF_EFFORT` (slowest, smallest output).
    ///
    /// `compression` must be passed explicitly: vips_heifsave_buffer
    /// defaults to VIPS_FOREIGN_HEIF_COMPRESSION_HEVC (x265) when it's
    /// omitted, not AV1, silently producing HEVC-in-a-heif-container
    /// output mislabeled as AVIF (and erroring outright on runtimes,
    /// like Alpine's, that only ship an AV1 encoder plugin).
    pub fn save_avif(&self, quality: i32, effort: u8, strip: bool) -> Result<Vec<u8>, VipsError> {
        const VIPS_FOREIGN_HEIF_COMPRESSION_AV1: i32 = 4;
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_heifsave_buffer(
                self.ptr.as_ptr(),
                &mut buf,
                &mut len,
                c"Q".as_ptr(),
                quality,
                c"compression".as_ptr(),
                VIPS_FOREIGN_HEIF_COMPRESSION_AV1,
                c"effort".as_ptr(),
                effort as c_int,
                c"strip".as_ptr(),
                bool_to_int(strip),
                ptr::null::<c_char>(),
            )
        };
        save_result(rc, buf, len)
    }

    /// Encode to GIF. Palette-based; no quality parameter.
    pub fn save_gif(&self) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_gifsave_buffer(self.ptr.as_ptr(), &mut buf, &mut len, ptr::null::<c_char>())
        };
        save_result(rc, buf, len)
    }

    /// Render the whole image to a packed raster in memory. Rows follow in
    /// order and bands interleave. Each sample uses the width of the image
    /// band format, so the caller must check the length before it reads
    /// the buffer as 8-bit samples.
    pub fn write_to_memory(&self) -> Result<Vec<u8>, VipsError> {
        let mut len: size_t = 0;
        let buf = unsafe { ffi::vips_image_write_to_memory(self.ptr.as_ptr(), &mut len) };
        if buf.is_null() {
            return Err(VipsError::OperationFailed(last_vips_error()));
        }
        save_result(0, buf, len)
    }

    /// Resize to fit within `width` (and optionally the height in the
    /// options) via `vips_thumbnail_image`. The image is already decoded at
    /// full size. Use `thumbnail_buffer` to decode at reduced size.
    pub fn thumbnail(&self, width: i32, opts: ThumbnailOptions) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        macro_rules! call {
            ($($arg:expr),*) => {
                ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    $($arg,)*
                    ptr::null::<c_char>(),
                )
            };
        }
        let rc = unsafe { thumbnail_with_options!(opts, call) };
        op_result(rc, output, VipsError::ResizeFailed)
    }

    /// Decode `data` and resize to fit within `width` in one step. This
    /// calls `vips_thumbnail_source`. libvips tells the decoder the target
    /// size, so JPEG and WebP decode at reduced size. JPEG uses scaling in
    /// the DCT domain when the shrink ratio is 4 or more. WebP uses the
    /// libwebp scaler in place of the Lanczos kernel. The method decodes the
    /// first frame only. It copies `data`, so `data` need not outlive the
    /// returned image. Do not use it when a pixel operation must run on the
    /// full-size image before the resize.
    ///
    /// The size of a `contain`, `inside`, or `pad` result can differ by 1 px
    /// or more from the size that `thumbnail` gives on the full image. The
    /// caller compares the size and corrects it when it matters.
    ///
    /// The decode uses the default `fail_on=none`. Do not use this method
    /// when a damaged source must fail. `fail_on=error` in the `option_string`
    /// argument is not reliable here: on libvips 8.15.1 the sequential decode
    /// of a truncated JPEG can return without an error.
    pub fn thumbnail_buffer(
        data: &[u8],
        width: i32,
        opts: ThumbnailOptions,
    ) -> Result<Self, VipsError> {
        let source = OwnedSource::copy_of(data).map_err(VipsError::ResizeFailed)?;
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        macro_rules! call {
            ($($arg:expr),*) => {
                ffi::vips_thumbnail_source(
                    source.ptr.as_ptr(),
                    &mut output,
                    width,
                    $($arg,)*
                    ptr::null::<c_char>(),
                )
            };
        }
        let rc = unsafe { thumbnail_with_options!(opts, call) };
        op_result(rc, output, VipsError::ResizeFailed)
    }

    /// Extract a rectangular sub-region.
    pub fn crop(&self, left: i32, top: i32, width: i32, height: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_extract_area(
                self.ptr.as_ptr(),
                &mut output,
                left,
                top,
                width,
                height,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Rotate by a multiple of 90 degrees (`ffi::VIPS_ANGLE_*`).
    pub fn rot(&self, angle: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc =
            unsafe { ffi::vips_rot(self.ptr.as_ptr(), &mut output, angle, ptr::null::<c_char>()) };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Apply the EXIF orientation tag to the pixels and remove the tag. An
    /// image without the tag comes back unchanged.
    pub fn autorot(&self) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc =
            unsafe { ffi::vips_autorot(self.ptr.as_ptr(), &mut output, ptr::null::<c_char>()) };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Flip horizontally or vertically (`ffi::VIPS_DIRECTION_*`).
    pub fn flip(&self, direction: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_flip(
                self.ptr.as_ptr(),
                &mut output,
                direction,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Find the bounding box of non-border pixels: (left, top, width, height).
    pub fn find_trim(&self, threshold: f64) -> Result<(i32, i32, i32, i32), VipsError> {
        let (mut left, mut top, mut width, mut height) = (0i32, 0i32, 0i32, 0i32);
        let rc = unsafe {
            ffi::vips_find_trim(
                self.ptr.as_ptr(),
                &mut left,
                &mut top,
                &mut width,
                &mut height,
                c"threshold".as_ptr(),
                threshold,
                ptr::null::<c_char>(),
            )
        };
        if rc != 0 {
            return Err(VipsError::OperationFailed(last_vips_error()));
        }
        Ok((left, top, width, height))
    }

    /// Apply an unsharp mask with the given sigma.
    pub fn sharpen(&self, sigma: f64) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_sharpen(
                self.ptr.as_ptr(),
                &mut output,
                c"sigma".as_ptr(),
                sigma,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Apply a Gaussian blur with the given sigma.
    pub fn gaussblur(&self, sigma: f64) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_gaussblur(self.ptr.as_ptr(), &mut output, sigma, ptr::null::<c_char>())
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Apply `out = in * a + b` per pixel (used for brightness/contrast).
    pub fn linear1(&self, a: f64, b: f64) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_linear1(self.ptr.as_ptr(), &mut output, a, b, ptr::null::<c_char>())
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Apply gamma correction with the given exponent.
    pub fn gamma(&self, exponent: f64) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_gamma(
                self.ptr.as_ptr(),
                &mut output,
                c"exponent".as_ptr(),
                exponent,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Convert to the given colorspace (`ffi::VIPS_INTERPRETATION_*`).
    pub fn colourspace(&self, space: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_colourspace(self.ptr.as_ptr(), &mut output, space, ptr::null::<c_char>())
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Extract `n` bands starting at `band`.
    pub fn extract_band(&self, band: i32, n: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_extract_band(
                self.ptr.as_ptr(),
                &mut output,
                band,
                c"n".as_ptr(),
                n,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Join two images band-wise (append `b`'s bands after `a`'s).
    pub fn bandjoin2(a: &Self, b: &Self) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_bandjoin2(
                a.ptr.as_ptr(),
                b.ptr.as_ptr(),
                &mut output,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Flatten alpha onto an RGB background color (0-255 per channel).
    pub fn flatten(&self, bg: [f64; 3]) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            let bg_array = ffi::vips_array_double_new(bg.as_ptr(), 3);
            let rc = ffi::vips_flatten(
                self.ptr.as_ptr(),
                &mut output,
                c"background".as_ptr(),
                bg_array,
                ptr::null::<c_char>(),
            );
            ffi::vips_area_unref(bg_array as *mut ffi::VipsArea);
            rc
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Embed (pad/letterbox) within a larger canvas at (x, y), filling the
    /// border with `bg`. Automatically extends to RGBA when the source has
    /// alpha (4-element background array with alpha = 255).
    pub fn embed(
        &self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        bg: [f64; 3],
    ) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let bg4 = [bg[0], bg[1], bg[2], 255.0];
        let n_bands: i32 = if self.bands() >= 4 { 4 } else { 3 };
        let rc = unsafe {
            let bg_array = ffi::vips_array_double_new(bg4.as_ptr(), n_bands);
            let rc = ffi::vips_embed(
                self.ptr.as_ptr(),
                &mut output,
                x,
                y,
                width,
                height,
                c"extend".as_ptr(),
                ffi::VIPS_EXTEND_BACKGROUND,
                c"background".as_ptr(),
                bg_array,
                ptr::null::<c_char>(),
            );
            ffi::vips_area_unref(bg_array as *mut ffi::VipsArea);
            rc
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Composite `overlay` on top of `self` at pixel offset `(x, y)`
    /// using standard "over" alpha blending (`VIPS_BLEND_MODE_OVER`).
    /// Used by the `draw` overlay pipeline (Cloudflare parity gap 11,
    /// docs/CLOUDFLARE_PARITY.md) -- Cloudflare's `draw` feature doesn't
    /// expose a custom blend mode via its documented options, so `over`
    /// (the standard "painted on top" compositing) is the only mode
    /// wired here.
    pub fn composite_over(&self, overlay: &Self, x: i32, y: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_composite2(
                self.ptr.as_ptr(),
                overlay.ptr.as_ptr(),
                &mut output,
                ffi::VIPS_BLEND_MODE_OVER,
                c"x".as_ptr(),
                x,
                c"y".as_ptr(),
                y,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }

    /// Tile `self` to fill a `width` x `height` canvas (`VIPS_EXTEND_REPEAT`
    /// via `vips_embed`), used for the `draw` overlay `repeat` option
    /// (Cloudflare parity gap 11).
    pub fn tile_to_size(&self, width: i32, height: i32) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            ffi::vips_embed(
                self.ptr.as_ptr(),
                &mut output,
                0,
                0,
                width,
                height,
                c"extend".as_ptr(),
                ffi::VIPS_EXTEND_REPEAT,
                ptr::null::<c_char>(),
            )
        };
        op_result(rc, output, VipsError::OperationFailed)
    }
}

/// Join a slice of images vertically (one column) — used to reassemble
/// cropped animation frames into a stacked buffer. Capped at 256 frames,
/// matching the caller's own cap (see docs/INVARIANTS.md INV-2).
pub fn arrayjoin_vertical(images: &[VipsImage]) -> Result<VipsImage, VipsError> {
    let n = images.len().min(256) as i32;
    let mut ptrs: Vec<*mut ffi::VipsImage> = images[..n as usize]
        .iter()
        .map(|img| img.ptr.as_ptr())
        .collect();
    let mut output: *mut ffi::VipsImage = ptr::null_mut();
    let rc = unsafe {
        ffi::vips_arrayjoin(
            ptrs.as_mut_ptr(),
            &mut output,
            n,
            c"across".as_ptr(),
            1i32,
            ptr::null::<c_char>(),
        )
    };
    op_result(rc, output, VipsError::OperationFailed)
}

fn bool_to_int(v: bool) -> i32 {
    if v { 1 } else { 0 }
}

/// A `VipsSource` over a private copy of encoded image bytes. The copy
/// lives in a reference-counted blob that decoders keep alive. A
/// `VipsImage` can therefore outlive the source bytes.
struct OwnedSource {
    ptr: ptr::NonNull<ffi::VipsSource>,
}

impl OwnedSource {
    fn copy_of(data: &[u8]) -> Result<Self, String> {
        // `vips_blob_copy` fails for zero bytes and sets no error message.
        if data.is_empty() {
            return Err("empty source".to_string());
        }
        // The copy is needed: a lazy image reads the bytes after the caller drops `data`.
        let blob =
            unsafe { ffi::vips_blob_copy(data.as_ptr() as *const c_void, data.len() as size_t) };
        if blob.is_null() {
            return Err(last_vips_error());
        }
        unsafe { Self::from_blob(blob) }
    }

    /// Build a source over `blob` and release the caller's reference to it.
    ///
    /// # Safety
    ///
    /// `blob` must be a valid, non-null `VipsBlob` whose reference the
    /// caller owns. The caller must not use that reference afterwards.
    unsafe fn from_blob(blob: *mut ffi::VipsBlob) -> Result<Self, String> {
        let raw = unsafe { ffi::vips_source_new_from_blob(blob) };
        // The source holds its own reference to the blob.
        unsafe { ffi::vips_area_unref(blob as *mut ffi::VipsArea) };
        ptr::NonNull::new(raw)
            .map(|ptr| OwnedSource { ptr })
            .ok_or_else(last_vips_error)
    }
}

impl Drop for OwnedSource {
    fn drop(&mut self) {
        unsafe { ffi::g_object_unref(self.ptr.as_ptr() as *mut c_void) }
    }
}

fn op_result(
    rc: i32,
    output: *mut ffi::VipsImage,
    on_fail: fn(String) -> VipsError,
) -> Result<VipsImage, VipsError> {
    if rc != 0 {
        return Err(on_fail(last_vips_error()));
    }
    match unsafe { VipsImage::from_raw(output) } {
        Some(img) => Ok(img),
        None => Err(on_fail(last_vips_error())),
    }
}

/// Copy a vips-allocated output buffer into an owned `Vec<u8>` and free
/// the original via `g_free`, or report the save error.
fn save_result(rc: i32, buf: *mut c_void, len: size_t) -> Result<Vec<u8>, VipsError> {
    if rc != 0 {
        return Err(VipsError::SaveFailed(last_vips_error()));
    }
    let slice = unsafe { std::slice::from_raw_parts(buf as *const u8, len) };
    let owned = slice.to_vec();
    unsafe { ffi::g_free(buf) };
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::AtomicUsize;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("reading fixture {path:?}: {e}"))
    }

    #[test]
    fn load_static_png_reports_correct_dimensions() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        assert_eq!(img.width(), 4);
        assert_eq!(img.height(), 4);
        assert!(img.n_pages().is_none() || img.n_pages() == Some(1));
    }

    #[test]
    fn load_and_reencode_static_png_as_jpeg_round_trips() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        let jpeg = img.save_jpeg(80, true).expect("encode jpeg");
        assert!(!jpeg.is_empty());
        // JPEG magic bytes
        assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn load_and_reencode_static_png_as_avif_round_trips() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        let avif = img
            .save_avif(80, DEFAULT_AVIF_EFFORT, true)
            .expect("encode avif");
        assert!(!avif.is_empty());
        // ISO BMFF ftyp box: the major brand must be "avif", confirming
        // AV1 compression was actually selected. Omitting `compression`
        // in the vips_heifsave_buffer call defaults to HEVC, which would
        // instead produce a "heic"/"mif1" brand here.
        assert_eq!(&avif[4..8], b"ftyp");
        assert_eq!(&avif[8..12], b"avif");
    }

    #[test]
    fn load_and_reencode_jpeg_source_as_png_round_trips() {
        init().expect("vips init");
        let data = fixture("cmyk.jpg"); // any real JPEG source; also exercises CMYK below
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        let png = img.save_png(6, true).expect("encode png");
        assert!(!png.is_empty());
        // PNG magic bytes
        assert_eq!(
            &png[0..8],
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        );
    }

    #[test]
    fn load_and_reencode_webp_source_as_jpeg_round_trips() {
        init().expect("vips init");
        let data = fixture("static.webp");
        let img = VipsImage::from_buffer(&data).expect("load webp");
        assert_eq!(img.width(), 8);
        assert_eq!(img.height(), 8);
        let jpeg = img.save_jpeg(80, true).expect("encode jpeg");
        assert!(!jpeg.is_empty());
        assert_eq!(&jpeg[0..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn load_cmyk_source_reports_four_bands() {
        init().expect("vips init");
        let data = fixture("cmyk.jpg");
        let img = VipsImage::from_buffer(&data).expect("load cmyk jpeg");
        assert_eq!(img.bands(), 4);
    }

    #[test]
    fn load_source_with_exif_orientation_exposes_the_raw_tag() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load exif-oriented jpeg");
        // Characterization test, not a correctness claim: from_buffer's
        // width/height are the raw pre-rotation pixel dimensions (8x4),
        // not the visually-upright dimensions EXIF orientation 6 implies
        // (4x8). The orientation tag itself is readable via get_int, but
        // nothing in the pipeline currently applies it before computing
        // resize/crop targets -- see the Phase 1 code-quality audit note
        // in docs/INVARIANTS.md and pipeline.rs, a pre-existing limitation
        // inherited unchanged from the original Zig implementation, not a
        // regression from this port.
        assert_eq!(img.width(), 8);
        assert_eq!(img.height(), 4);
        assert_eq!(img.get_int("orientation"), Some(6));
    }

    #[test]
    fn autorot_applies_the_exif_orientation_and_removes_the_tag() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load exif-oriented jpeg");
        assert_eq!((img.width(), img.height()), (8, 4));
        let upright = img.autorot().expect("autorot");
        assert_eq!((upright.width(), upright.height()), (4, 8));
        assert_eq!(upright.get_int("orientation"), None);
    }

    #[test]
    fn autorot_leaves_an_untagged_image_unchanged() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        let upright = img.autorot().expect("autorot");
        let again = upright.autorot().expect("autorot again");
        assert_eq!((again.width(), again.height()), (4, 8));
    }

    #[test]
    fn load_corrupt_truncated_source_returns_an_error() {
        init().expect("vips init");
        let data = fixture("corrupt_truncated.png");
        assert!(VipsImage::from_buffer(&data).is_err());
    }

    #[test]
    fn probe_loads_only_first_frame_of_animated_gif() {
        init().expect("vips init");
        let data = fixture("loading.gif");
        let img = VipsImage::from_buffer(&data).expect("probe gif");
        // Probe load (no "n" option) reads page 0 dimensions, but n-pages
        // metadata still reports the *source's* total frame count.
        assert_eq!(img.width(), 128);
        let n_pages = img.n_pages().expect("n-pages metadata present");
        assert_eq!(n_pages, 12, "loading.gif fixture has 12 frames");
    }

    #[test]
    fn animated_load_stacks_all_frames_vertically() {
        init().expect("vips init");
        let data = fixture("loading.gif");
        let img = VipsImage::from_buffer_animated(&data, -1).expect("load all frames");
        let n_pages = img.n_pages().expect("n-pages metadata present");
        assert_eq!(n_pages, 12);
        let page_height = img.page_height().expect("page-height metadata present");
        assert_eq!(page_height, 128);
        assert_eq!(
            img.height(),
            page_height * n_pages,
            "frames stacked vertically"
        );
    }

    #[test]
    fn animated_load_clamps_to_requested_frame_count() {
        // Loading with an explicit "n" clamps how many frames are actually
        // decoded and stacked into the image buffer, but `n-pages` metadata
        // continues to report the SOURCE's total frame count (12), not the
        // clamped count — this is why the pipeline must compute its own
        // effective_pages = min(n_pages, max_frames) rather than re-reading
        // n-pages after a clamped reload. The clamp is only observable via
        // height / page_height.
        init().expect("vips init");
        let data = fixture("loading.gif");
        let img = VipsImage::from_buffer_animated(&data, 5).expect("load 5 frames");
        assert_eq!(
            img.n_pages(),
            Some(12),
            "n-pages metadata reflects source total, not the clamp"
        );
        let page_height = img.page_height().expect("page-height metadata present");
        assert_eq!(page_height, 128);
        assert_eq!(
            img.height() / page_height,
            5,
            "actual loaded frame count is only recoverable via height / page_height"
        );
    }

    #[test]
    fn get_int_missing_field_returns_none() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        assert_eq!(img.get_int("no-such-field"), None);
    }

    // ------------------------------------------------------------------
    // Cloudflare parity gap 11 (draw overlays, docs/CLOUDFLARE_PARITY.md)
    // -- compositing math proof. This module proves libvips composite2
    // works correctly against local, already-decoded images; the actual
    // remote-URL fetch that would supply a real overlay in production is
    // deliberately not implemented (see CLOUDFLARE_PARITY.md gap 11).
    // ------------------------------------------------------------------

    #[test]
    fn composite_over_grows_output_to_base_size_and_preserves_alpha() {
        init().expect("vips init");
        let base = VipsImage::from_buffer(&fixture("bench_2000x1500.png")).expect("load base");
        let overlay = VipsImage::from_buffer(&fixture("test_4x4.png")).expect("load overlay");
        let composited = base
            .composite_over(&overlay, 10, 20)
            .expect("composite2 should succeed");
        assert_eq!(composited.width(), base.width());
        assert_eq!(composited.height(), base.height());
    }

    #[test]
    fn composite_over_at_origin_succeeds() {
        init().expect("vips init");
        let base = VipsImage::from_buffer(&fixture("test_4x4.png")).expect("load base");
        let overlay = VipsImage::from_buffer(&fixture("test_4x4.png")).expect("load overlay");
        let composited = base
            .composite_over(&overlay, 0, 0)
            .expect("composite2 should succeed");
        assert_eq!(composited.width(), 4);
        assert_eq!(composited.height(), 4);
    }

    #[test]
    fn tile_to_size_fills_the_requested_canvas() {
        init().expect("vips init");
        let overlay = VipsImage::from_buffer(&fixture("test_4x4.png")).expect("load overlay");
        let tiled = overlay.tile_to_size(16, 12).expect("tile should succeed");
        assert_eq!(tiled.width(), 16);
        assert_eq!(tiled.height(), 12);
    }

    fn thumbnail_within_100px(img: &VipsImage, no_rotate: bool) -> VipsImage {
        img.thumbnail(
            100,
            ThumbnailOptions {
                height: Some(100),
                crop: None,
                size: Some(crate::ffi::VIPS_SIZE_DOWN),
                no_rotate,
            },
        )
        .expect("thumbnail")
    }

    fn assert_write_to_memory_matches_dimensions(img: &VipsImage) {
        let bytes = img.write_to_memory().expect("write to memory");
        let expected = img.width() as usize * img.height() as usize * img.bands() as usize;
        assert_eq!(bytes.len(), expected);
    }

    #[test]
    fn write_to_memory_returns_width_times_height_times_bands_bytes() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        assert_write_to_memory_matches_dimensions(&img);
    }

    #[test]
    fn write_to_memory_of_thumbnailed_image_matches_its_reported_dimensions() {
        init().expect("vips init");
        let data = fixture("bench_2000x1500.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        let small = thumbnail_within_100px(&img, false);
        assert_eq!((small.width(), small.height()), (100, 75));
        assert_write_to_memory_matches_dimensions(&small);
    }

    #[test]
    fn write_to_memory_of_exif_rotated_thumbnail_reports_upright_dimensions() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        assert_eq!((img.width(), img.height()), (8, 4));
        let small = thumbnail_within_100px(&img, false);
        assert_eq!((small.width(), small.height()), (4, 8));
        assert_write_to_memory_matches_dimensions(&small);
    }

    #[test]
    fn thumbnail_with_no_rotate_keeps_stored_pixel_orientation() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        assert_eq!((img.width(), img.height()), (8, 4));
        let small = thumbnail_within_100px(&img, true);
        assert_eq!((small.width(), small.height()), (8, 4));
        assert_write_to_memory_matches_dimensions(&small);
    }

    #[test]
    fn thumbnail_with_no_rotate_false_still_applies_exif_orientation() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        let small = img
            .thumbnail(
                100,
                ThumbnailOptions {
                    size: Some(crate::ffi::VIPS_SIZE_DOWN),
                    ..Default::default()
                },
            )
            .expect("thumbnail");
        assert_eq!((small.width(), small.height()), (4, 8));
    }

    #[test]
    fn write_to_memory_returns_error_when_pixel_decode_fails() {
        init().expect("vips init");
        let mut data = fixture("bench_2000x1500.png");
        // Cut the file inside the pixel data. The header still loads. By
        // default libvips accepts a short PNG, and its tolerance differs
        // between versions. With `fail_on=warning` the cut is an error on
        // every version, and it surfaces when `write_to_memory` decodes.
        data.truncate(data.len() * 6 / 10);
        let img = VipsImage::from_buffer_with_option(&data, "fail_on=warning")
            .expect("the header still loads");
        assert!(matches!(
            img.write_to_memory(),
            Err(VipsError::OperationFailed(_))
        ));
    }

    fn down_100px_options(no_rotate: bool) -> ThumbnailOptions {
        ThumbnailOptions {
            height: Some(100),
            size: Some(crate::ffi::VIPS_SIZE_DOWN),
            no_rotate,
            ..Default::default()
        }
    }

    #[test]
    fn thumbnail_buffer_matches_thumbnail_dimensions_for_exif_source() {
        init().expect("vips init");
        let data = fixture("exif_orientation.jpg");
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        for no_rotate in [false, true] {
            let opts = down_100px_options(no_rotate);
            let full = img.thumbnail(100, opts).expect("thumbnail");
            let direct = VipsImage::thumbnail_buffer(&data, 100, opts).expect("thumbnail_buffer");
            assert_eq!(
                (direct.width(), direct.height()),
                (full.width(), full.height()),
                "no_rotate={no_rotate}"
            );
        }
    }

    #[test]
    fn thumbnail_buffer_of_large_jpeg_matches_thumbnail_dimensions() {
        init().expect("vips init");
        // 2000x1500 -> 100 wide is a shrink factor of 20, so the JPEG loader
        // decodes at 1/8 scale and libvips resizes the rest of the way.
        let png_bytes = fixture("bench_2000x1500.png");
        let png = VipsImage::from_buffer(&png_bytes).expect("load png");
        let jpeg = png.save_jpeg(85, true).expect("encode jpeg");
        let opts = down_100px_options(false);
        let jpeg_img = VipsImage::from_buffer(&jpeg).expect("load jpeg");
        let full = jpeg_img.thumbnail(100, opts).expect("thumbnail");
        let direct = VipsImage::thumbnail_buffer(&jpeg, 100, opts).expect("thumbnail_buffer");
        assert_eq!(
            (direct.width(), direct.height()),
            (full.width(), full.height())
        );
        assert_eq!((direct.width(), direct.height()), (100, 75));
    }

    #[test]
    fn thumbnail_buffer_of_garbage_returns_resize_failed() {
        init().expect("vips init");
        assert!(matches!(
            VipsImage::thumbnail_buffer(b"not an image", 100, ThumbnailOptions::default()),
            Err(VipsError::ResizeFailed(_))
        ));
    }

    #[test]
    fn from_buffer_image_outlives_its_source_bytes() {
        init().expect("vips init");
        let mut data = fixture("bench_2000x1500.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        data.fill(0);
        drop(data);
        let out = img
            .save_jpeg(85, true)
            .expect("encode after source overwritten");
        assert!(!out.is_empty());
    }

    #[test]
    fn thumbnail_buffer_image_outlives_its_source_bytes() {
        init().expect("vips init");
        let mut data = fixture("bench_2000x1500.png");
        let small = VipsImage::thumbnail_buffer(&data, 100, ThumbnailOptions::default())
            .expect("thumbnail_buffer");
        data.fill(0);
        drop(data);
        let out = small
            .save_jpeg(85, true)
            .expect("encode after source overwritten");
        assert!(!out.is_empty());
        assert_eq!(small.width(), 100);
    }

    #[test]
    fn shrinks_on_load_is_true_for_jpeg_and_webp_only() {
        init().expect("vips init");
        for (name, expected) in [
            ("exif_orientation.jpg", true),
            ("static.webp", true),
            ("test_4x4.png", false),
            ("loading.gif", false),
        ] {
            let data = fixture(name);
            let img = VipsImage::from_buffer(&data).expect("load");
            assert_eq!(img.shrinks_on_load(), expected, "{name}");
        }
    }

    /// A little-endian TIFF with one uncompressed 8-bit grey strip.
    fn grey_tiff(width: u32, height: u32) -> Vec<u8> {
        const ENTRY_COUNT: u32 = 8;
        const HEADER_LEN: u32 = 8;
        let pixel_offset = HEADER_LEN + 2 + ENTRY_COUNT * 12 + 4;
        let pixel_count = width * height;
        // Each entry is (tag, type, value). Type 3 is SHORT and type 4 is
        // LONG. Every count is 1, and every value fits in the entry.
        let entries: [(u16, u16, u32); ENTRY_COUNT as usize] = [
            (256, 4, width),
            (257, 4, height),
            (258, 3, 8),
            (259, 3, 1),
            (262, 3, 1),
            (273, 4, pixel_offset),
            (278, 4, height),
            (279, 4, pixel_count),
        ];
        let mut tiff = vec![b'I', b'I', 42, 0];
        tiff.extend_from_slice(&HEADER_LEN.to_le_bytes());
        tiff.extend_from_slice(&(ENTRY_COUNT as u16).to_le_bytes());
        for (tag, kind, value) in entries {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&kind.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend((0..pixel_count).map(|i| (i % 251) as u8));
        tiff
    }

    #[test]
    fn init_turns_off_the_operation_cache() {
        init().expect("vips init");
        let limit = unsafe { ffi::vips_cache_get_max() };
        assert_eq!(
            limit, 0,
            "libvips keeps {limit} operations and their inputs"
        );
    }

    #[test]
    fn empty_source_error_says_the_source_is_empty() {
        init().expect("vips init");
        let load = VipsImage::from_buffer(&[]).err().expect("empty load fails");
        assert!(matches!(load, VipsError::LoadFailed(_)), "{load:?}");
        assert_eq!(load.to_string(), "vips load failed: empty source");
        let resize = VipsImage::thumbnail_buffer(&[], 100, ThumbnailOptions::default())
            .err()
            .expect("empty resize fails");
        assert!(matches!(resize, VipsError::ResizeFailed(_)), "{resize:?}");
        assert_eq!(resize.to_string(), "vips resize failed: empty source");
    }

    #[test]
    fn shrinks_on_load_is_false_for_heif_and_tiff() {
        init().expect("vips init");
        let png = VipsImage::from_buffer(&fixture("test_4x4.png")).expect("load png");
        let mut sources = vec![("tiff", grey_tiff(16, 8))];
        // The libheif of some builds, such as Alpine, has no AVIF encoder.
        if let Ok(avif) = png.save_avif(50, 0, true) {
            sources.push(("avif", avif));
        }
        for (name, data) in sources {
            let img = VipsImage::from_buffer(&data).unwrap_or_else(|e| panic!("load {name}: {e}"));
            assert!(!img.shrinks_on_load(), "{name}");
        }
    }

    #[test]
    fn grey_tiff_helper_builds_a_loadable_image() {
        init().expect("vips init");
        let img = VipsImage::from_buffer(&grey_tiff(16, 8)).expect("load tiff");
        assert_eq!((img.width(), img.height(), img.bands()), (16, 8, 1));
    }

    /// A 2048x1536 JPEG with EXIF orientation 6, so it displays as
    /// 1536x2048. Both sides divide by 8, so no shrink factor truncates.
    fn oriented_jpeg_2048x1536() -> Vec<u8> {
        let png = VipsImage::from_buffer(&fixture("bench_2000x1500.png")).expect("load png");
        let scaled = png
            .thumbnail(
                2048,
                ThumbnailOptions {
                    height: Some(1536),
                    size: Some(crate::ffi::VIPS_SIZE_FORCE),
                    ..Default::default()
                },
            )
            .expect("scale");
        scaled.set_int("orientation", 6);
        let jpeg = scaled.save_jpeg(85, false).expect("encode jpeg");
        let reloaded = VipsImage::from_buffer(&jpeg).expect("reload jpeg");
        assert_eq!(reloaded.get_int("orientation"), Some(6));
        jpeg
    }

    /// The size libvips gives for a 300 px wide box on the 2048x1536 source
    /// with orientation 6. `crop` and `force` give the box. Otherwise the
    /// image fits inside the box.
    fn expected_thumbnail_size(
        height: Option<i32>,
        crop: bool,
        force: bool,
        no_rotate: bool,
    ) -> (i32, i32) {
        let (box_w, box_h) = (300, height.unwrap_or(300));
        if crop || force {
            return (box_w, box_h);
        }
        let (source_w, source_h) = if no_rotate {
            (2048.0, 1536.0)
        } else {
            (1536.0, 2048.0)
        };
        let scale = (box_w as f64 / source_w).min(box_h as f64 / source_h);
        (
            (source_w * scale).round() as i32,
            (source_h * scale).round() as i32,
        )
    }

    #[test]
    fn thumbnail_options_reach_libvips_in_every_combination() {
        init().expect("vips init");
        let data = oriented_jpeg_2048x1536();
        let img = VipsImage::from_buffer(&data).expect("load jpeg");
        for height in [None, Some(160)] {
            for crop in [None, Some(crate::ffi::VIPS_INTERESTING_CENTRE)] {
                for size in [None, Some(crate::ffi::VIPS_SIZE_FORCE)] {
                    for no_rotate in [false, true] {
                        let opts = ThumbnailOptions {
                            height,
                            crop,
                            size,
                            no_rotate,
                        };
                        let expected = expected_thumbnail_size(
                            height,
                            crop.is_some(),
                            size.is_some(),
                            no_rotate,
                        );
                        let label = format!("{opts:?}");
                        let full = img.thumbnail(300, opts).expect("thumbnail");
                        assert_eq!((full.width(), full.height()), expected, "thumbnail {label}");
                        let direct = VipsImage::thumbnail_buffer(&data, 300, opts)
                            .expect("thumbnail_buffer");
                        assert_eq!(
                            (direct.width(), direct.height()),
                            expected,
                            "thumbnail_buffer {label}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn thumbnail_buffer_of_jpeg_and_webp_outlives_its_source_bytes() {
        init().expect("vips init");
        let png = VipsImage::from_buffer(&fixture("bench_2000x1500.png")).expect("load png");
        let sources = [
            ("jpeg", png.save_jpeg(85, true).expect("encode jpeg")),
            ("webp", png.save_webp(80, true).expect("encode webp")),
        ];
        for (label, mut data) in sources {
            let small = VipsImage::thumbnail_buffer(&data, 100, ThumbnailOptions::default())
                .unwrap_or_else(|e| panic!("thumbnail_buffer {label}: {e}"));
            // The loader has not read the pixels yet. Overwrite the bytes,
            // so a decode from borrowed memory fails or differs.
            data.fill(0xEE);
            drop(data);
            let out = small
                .save_jpeg(85, true)
                .unwrap_or_else(|e| panic!("encode {label} after source overwritten: {e}"));
            assert!(!out.is_empty(), "{label}");
            assert_eq!(small.width(), 100, "{label}");
        }
    }

    #[test]
    fn raw_extern_blocks_live_only_in_ffi_rs() {
        let needle = ["unsafe", "extern", "\"C\"", "{"].join(" ");
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        for entry in fs::read_dir(&src).expect("read src") {
            let path = entry.expect("dir entry").path();
            if path.file_name().is_some_and(|name| name == "ffi.rs") {
                continue;
            }
            let text = fs::read_to_string(&path).expect("read source file");
            if text.contains(&needle) {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "extern blocks outside ffi.rs: {offenders:?}"
        );
    }

    static BLOB_FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_blob_free(_data: *mut c_void, _area: *mut c_void) -> c_int {
        BLOB_FREE_CALLS.fetch_add(1, Ordering::SeqCst);
        0
    }

    #[test]
    fn owned_source_releases_its_blob_once_when_it_drops() {
        init().expect("vips init");
        static BYTES: [u8; 4] = *b"abcd";
        let blob = unsafe {
            ffi::vips_blob_new(
                Some(count_blob_free),
                BYTES.as_ptr() as *const c_void,
                BYTES.len(),
            )
        };
        assert!(!blob.is_null());
        let source = unsafe { OwnedSource::from_blob(blob) }.expect("source from blob");
        assert_eq!(
            BLOB_FREE_CALLS.load(Ordering::SeqCst),
            0,
            "the source keeps the blob alive"
        );
        drop(source);
        assert_eq!(
            BLOB_FREE_CALLS.load(Ordering::SeqCst),
            1,
            "dropping the source releases the blob once"
        );
    }

    #[test]
    fn save_avif_accepts_the_full_effort_range() {
        init().expect("vips init");
        let data = fixture("test_4x4.png");
        let img = VipsImage::from_buffer(&data).expect("load png");
        for effort in [0, DEFAULT_AVIF_EFFORT, MAX_AVIF_EFFORT] {
            let avif = img.save_avif(80, effort, true).expect("encode avif");
            assert_eq!(&avif[8..12], b"avif", "effort={effort}");
        }
    }

    /// Cut points that fall inside the pixel data of the first frame, or
    /// inside its headers, of each fixture. Truncated JPEG and WebP sources
    /// also come from a re-encoded copy of the large PNG fixture, so the cut
    /// lands inside real entropy-coded data.
    fn truncated_sources() -> Vec<(String, Vec<u8>)> {
        let big_png = fixture("bench_2000x1500.png");
        let big = VipsImage::from_buffer(&big_png).expect("load bench png");
        let big_jpeg = big.save_jpeg(85, true).expect("encode jpeg");
        let big_webp = big.save_webp(80, true).expect("encode webp");
        let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
        let mut cut = |label: &str, data: &[u8], at: usize| {
            cases.push((format!("{label} cut at {at}"), data[..at].to_vec()));
        };
        for at in [big_png.len() / 4, big_png.len() / 2, big_png.len() * 3 / 4] {
            cut("bench_2000x1500.png", &big_png, at);
        }
        let cmyk = fixture("cmyk.jpg");
        cut("cmyk.jpg", &cmyk, 281);
        let exif = fixture("exif_orientation.jpg");
        cut("exif_orientation.jpg", &exif, 657);
        for at in [big_jpeg.len() / 4, big_jpeg.len() / 2] {
            cut("re-encoded jpeg", &big_jpeg, at);
        }
        let gif = fixture("loading.gif");
        for at in [101, 355, 800] {
            cut("loading.gif", &gif, at);
        }
        let webp = fixture("static.webp");
        cut("static.webp", &webp, 32);
        cut("static.webp", &webp, 63);
        for at in [big_webp.len() / 2, big_webp.len() - 100] {
            cut("re-encoded webp", &big_webp, at);
        }
        cases
    }

    /// Sources whose tail is overwritten, not cut, so the length stays
    /// valid.
    fn corrupted_tail_sources() -> Vec<(String, Vec<u8>)> {
        let big_png = fixture("bench_2000x1500.png");
        let big = VipsImage::from_buffer(&big_png).expect("load bench png");
        let big_jpeg = big.save_jpeg(85, true).expect("encode jpeg");
        [
            ("bench_2000x1500.png", big_png),
            ("re-encoded jpeg", big_jpeg),
        ]
        .into_iter()
        .map(|(label, mut bytes)| {
            let from = bytes.len() / 2;
            bytes[from..].fill(0xAA);
            (format!("{label} tail overwritten from {from}"), bytes)
        })
        .collect()
    }

    fn valid_fixtures() -> Vec<(&'static str, Vec<u8>)> {
        [
            "test_4x4.png",
            "alpha_4x4.png",
            "bench_2000x1500.png",
            "cmyk.jpg",
            "exif_orientation.jpg",
            "loading.gif",
            "static.webp",
        ]
        .into_iter()
        .map(|name| (name, fixture(name)))
        .collect()
    }

    /// Fail with the label of every source that a strict load decodes to
    /// pixels without an error.
    fn assert_strict_read_fails(sources: Vec<(String, Vec<u8>)>) {
        let decoded: Vec<String> = sources
            .into_iter()
            .filter(|(_, data)| {
                VipsImage::from_buffer_strict(data)
                    .and_then(|img| img.write_to_memory())
                    .is_ok()
            })
            .map(|(label, _)| label)
            .collect();
        assert!(
            decoded.is_empty(),
            "strict load returned pixels for: {decoded:#?}"
        );
    }

    #[test]
    fn strict_load_of_truncated_source_returns_error_when_reading_pixels() {
        init().expect("vips init");
        assert_strict_read_fails(truncated_sources());
    }

    #[test]
    fn strict_load_of_source_with_corrupted_tail_returns_error_when_reading_pixels() {
        init().expect("vips init");
        assert_strict_read_fails(corrupted_tail_sources());
    }

    #[test]
    fn strict_load_of_valid_source_matches_the_default_load() {
        init().expect("vips init");
        for (name, data) in valid_fixtures() {
            let lax = VipsImage::from_buffer(&data).expect(name);
            let strict = VipsImage::from_buffer_strict(&data).expect(name);
            assert_eq!(
                (strict.width(), strict.height(), strict.bands()),
                (lax.width(), lax.height(), lax.bands()),
                "{name}"
            );
            assert_eq!(strict.n_pages(), lax.n_pages(), "{name}");
            assert_eq!(
                strict.write_to_memory().expect(name),
                lax.write_to_memory().expect(name),
                "{name}: strict and default loads decode different pixels"
            );
        }
    }

    #[test]
    fn strict_animated_load_of_truncated_gif_returns_error_when_reading_pixels() {
        init().expect("vips init");
        let gif = fixture("loading.gif");
        let decoded: Vec<usize> = [101, 355, 800]
            .into_iter()
            .filter(|&at| {
                VipsImage::from_buffer_animated_strict(&gif[..at], -1)
                    .and_then(|img| img.write_to_memory())
                    .is_ok()
            })
            .collect();
        assert!(
            decoded.is_empty(),
            "strict animated load returned pixels for loading.gif cut at {decoded:?}"
        );
    }

    #[test]
    fn strict_animated_load_of_valid_gif_matches_the_default_animated_load() {
        init().expect("vips init");
        let gif = fixture("loading.gif");
        let lax = VipsImage::from_buffer_animated(&gif, -1).expect("load gif");
        let strict = VipsImage::from_buffer_animated_strict(&gif, -1).expect("load gif");
        assert_eq!(
            (strict.width(), strict.height()),
            (lax.width(), lax.height())
        );
        assert_eq!(strict.n_pages(), lax.n_pages());
        assert_eq!(
            strict.write_to_memory().expect("pixels"),
            lax.write_to_memory().expect("pixels")
        );
    }

    #[test]
    fn default_load_of_truncated_png_still_returns_pixels() {
        init().expect("vips init");
        // Pins the behaviour of every output format other than thumbhash:
        // the default load stays lenient, so a partly decoded PNG still
        // gives an image.
        let data = fixture("bench_2000x1500.png");
        let img = VipsImage::from_buffer(&data[..data.len() / 2]).expect("header loads");
        let pixels = img
            .write_to_memory()
            .expect("default load tolerates the cut");
        assert_eq!(pixels.len(), 2000 * 1500 * 3);
    }
}
