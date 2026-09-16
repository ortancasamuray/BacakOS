use super::ffi::*;
use std::{ops, ptr, slice};

pub struct Frame {
    surface: IOSurfaceRef,
    inner: &'static [u8],
    bytes_per_row: usize
}

impl Frame {
    pub unsafe fn new(surface: IOSurfaceRef) -> Frame {
        CFRetain(surface);
        IOSurfaceIncrementUseCount(surface);

        IOSurfaceLock(
            surface,
            SURFACE_LOCK_READ_ONLY,
            ptr::null_mut()
        );

        let inner = slice::from_raw_parts(
            IOSurfaceGetBaseAddress(surface) as *const u8,
            IOSurfaceGetAllocSize(surface)
        );
        let bytes_per_row = IOSurfaceGetBytesPerRow(surface);

        Frame { surface, inner, bytes_per_row }
    }

    /// The real per-row stride (`IOSurfaceGetBytesPerRow`) — NOT the same
    /// as `self.len() / height`, which only holds if `IOSurfaceGetAllocSize`
    /// happens to be an exact multiple of the row count (see this file's
    /// `ffi.rs` doc comment on `IOSurfaceGetBytesPerRow`).
    pub fn stride(&self) -> usize {
        self.bytes_per_row
    }
}

impl ops::Deref for Frame {
    type Target = [u8];
    fn deref<'a>(&'a self) -> &'a [u8] {
        self.inner
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            IOSurfaceUnlock(
                self.surface,
                SURFACE_LOCK_READ_ONLY,
                ptr::null_mut()
            );

            IOSurfaceDecrementUseCount(self.surface);
            CFRelease(self.surface);
        }
    }
}
