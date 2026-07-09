use std::ffi::CStr;
use std::fmt;

use crate::ffi;

/// Errors surfaced from libvips FFI calls.
///
/// Mirrors `src/vips/bindings.zig`'s `VipsError` set. Every variant carries
/// the last message from `vips_error_buffer()` at the point of failure.
#[derive(Debug)]
pub enum VipsError {
    InitFailed(String),
    LoadFailed(String),
    SaveFailed(String),
    ResizeFailed(String),
    OperationFailed(String),
}

impl fmt::Display for VipsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VipsError::InitFailed(m) => write!(f, "vips init failed: {m}"),
            VipsError::LoadFailed(m) => write!(f, "vips load failed: {m}"),
            VipsError::SaveFailed(m) => write!(f, "vips save failed: {m}"),
            VipsError::ResizeFailed(m) => write!(f, "vips resize failed: {m}"),
            VipsError::OperationFailed(m) => write!(f, "vips operation failed: {m}"),
        }
    }
}

impl std::error::Error for VipsError {}

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
