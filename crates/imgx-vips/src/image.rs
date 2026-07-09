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

    /// Write an integer metadata field.
    pub fn set_int(&mut self, name: &str, value: i32) {
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
    pub fn save_jpeg(&self, quality: i32) -> Result<Vec<u8>, VipsError> {
        let q_key = CString::new("Q").unwrap();
        let mut buf: *mut c_void = ptr::null_mut();
        let mut len: size_t = 0;
        let rc = unsafe {
            ffi::vips_jpegsave_buffer(
                self.ptr.as_ptr(),
                &mut buf,
                &mut len,
                q_key.as_ptr(),
                quality,
                ptr::null::<c_char>(),
            )
        };
        save_result(rc, buf, len)
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
        let jpeg = img.save_jpeg(80).expect("encode jpeg");
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
