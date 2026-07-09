use std::ffi::{c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use libc::{c_char, size_t};

use crate::error::{last_vips_error, VipsError};
use crate::ffi;

static VIPS_INIT: Once = Once::new();
static VIPS_INIT_OK: AtomicBool = AtomicBool::new(false);

/// Initialize libvips. Safe to call more than once (subsequent calls are
/// no-ops); mirrors `bindings.zig`'s `init()`. Must be paired with at most
/// one `shutdown()` call, from `main`, never from tests.
pub fn init() -> Result<(), VipsError> {
    VIPS_INIT.call_once(|| {
        let argv0 = CString::new("imgx").unwrap();
        let rc = unsafe { ffi::vips_init(argv0.as_ptr()) };
        VIPS_INIT_OK.store(rc == 0, Ordering::SeqCst);
    });
    if VIPS_INIT_OK.load(Ordering::SeqCst) {
        Ok(())
    } else {
        Err(VipsError::InitFailed(last_vips_error()))
    }
}

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
}

impl Drop for VipsImage {
    fn drop(&mut self) {
        unsafe { ffi::g_object_unref(self.ptr.as_ptr() as *mut c_void) }
    }
}

impl VipsImage {
    /// Load an image from an in-memory buffer, first frame/page only
    /// (the "probe" load — cheap, used to detect animation metadata).
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

    fn from_buffer_with_option(data: &[u8], option_string: &str) -> Result<Self, VipsError> {
        let c_opts = CString::new(option_string).map_err(|_| {
            VipsError::LoadFailed("option string contained an interior NUL".to_string())
        })?;
        let raw = unsafe {
            ffi::vips_image_new_from_buffer(
                data.as_ptr() as *const c_void,
                data.len() as size_t,
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

    pub fn width(&self) -> i32 {
        unsafe { ffi::vips_image_get_width(self.ptr.as_ptr()) }.max(0)
    }

    pub fn height(&self) -> i32 {
        unsafe { ffi::vips_image_get_height(self.ptr.as_ptr()) }.max(0)
    }

    pub fn bands(&self) -> i32 {
        unsafe { ffi::vips_image_get_bands(self.ptr.as_ptr()) }.max(0)
    }

    pub fn has_alpha(&self) -> bool {
        unsafe { ffi::vips_image_hasalpha(self.ptr.as_ptr()) != 0 }
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

    /// Encode to AVIF (via the HEIF encoder). `quality` is 1-100.
    pub fn save_avif(&self, quality: i32, strip: bool) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_heifsave_buffer(
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

    /// Encode to GIF. Palette-based; no quality parameter.
    pub fn save_gif(&self) -> Result<Vec<u8>, VipsError> {
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_gifsave_buffer(self.ptr.as_ptr(), &mut buf, &mut len, ptr::null::<c_char>())
        };
        save_result(rc, buf, len)
    }

    /// Resize to fit within `width` (and optionally the options' height)
    /// via `vips_thumbnail_image`.
    pub fn thumbnail(&self, width: i32, opts: ThumbnailOptions) -> Result<Self, VipsError> {
        let mut output: *mut ffi::VipsImage = ptr::null_mut();
        let rc = unsafe {
            match (opts.height, opts.crop, opts.size) {
                (Some(h), Some(crop), Some(size)) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"height".as_ptr(),
                    h,
                    c"crop".as_ptr(),
                    crop,
                    c"size".as_ptr(),
                    size,
                    ptr::null::<c_char>(),
                ),
                (Some(h), Some(crop), None) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"height".as_ptr(),
                    h,
                    c"crop".as_ptr(),
                    crop,
                    ptr::null::<c_char>(),
                ),
                (Some(h), None, Some(size)) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"height".as_ptr(),
                    h,
                    c"size".as_ptr(),
                    size,
                    ptr::null::<c_char>(),
                ),
                (Some(h), None, None) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"height".as_ptr(),
                    h,
                    ptr::null::<c_char>(),
                ),
                (None, Some(crop), Some(size)) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"crop".as_ptr(),
                    crop,
                    c"size".as_ptr(),
                    size,
                    ptr::null::<c_char>(),
                ),
                (None, Some(crop), None) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"crop".as_ptr(),
                    crop,
                    ptr::null::<c_char>(),
                ),
                (None, None, Some(size)) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    c"size".as_ptr(),
                    size,
                    ptr::null::<c_char>(),
                ),
                (None, None, None) => ffi::vips_thumbnail_image(
                    self.ptr.as_ptr(),
                    &mut output,
                    width,
                    ptr::null::<c_char>(),
                ),
            }
        };
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
    if v {
        1
    } else {
        0
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
}
