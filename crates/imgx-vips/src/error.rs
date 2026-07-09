use std::ffi::CStr;

use thiserror::Error;

use crate::ffi;

/// Errors surfaced from libvips FFI calls.
///
/// Mirrors `src/vips/bindings.zig`'s `VipsError` set. Every variant carries
/// the last message from `vips_error_buffer()` at the point of failure.
#[derive(Debug, Error)]
pub enum VipsError {
    #[error("vips init failed: {0}")]
    InitFailed(String),
    #[error("vips load failed: {0}")]
    LoadFailed(String),
    #[error("vips save failed: {0}")]
    SaveFailed(String),
    #[error("vips resize failed: {0}")]
    ResizeFailed(String),
    #[error("vips operation failed: {0}")]
    OperationFailed(String),
}

/// Read and clear the thread-local vips error buffer.
pub(crate) fn last_vips_error() -> String {
    unsafe {
        let ptr = ffi::vips_error_buffer();
        let msg = if ptr.is_null() {
            String::new()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().trim().to_string()
        };
        ffi::vips_error_clear();
        msg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_message() {
        let err = VipsError::LoadFailed("bad buffer".to_string());
        assert_eq!(err.to_string(), "vips load failed: bad buffer");
    }
}
