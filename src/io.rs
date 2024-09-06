use core::{
    alloc::{GlobalAlloc, Layout},
    fmt::Debug,
};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, WIN32_ERROR};

#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

impl OwnedHandle {
    pub unsafe fn from_raw_handle(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub fn as_raw_handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

/// Win32 error with error code.
pub struct Error(pub WIN32_ERROR);

impl Error {
    /// Create [`Error`] from [`GetLastError`].
    pub fn last_os_error() -> Self {
        Self(unsafe { GetLastError() })
    }
}

impl Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::result::Result<(), core::fmt::Error> {
        errno::Errno(self.0 as _).fmt(f)
    }
}

/// Win32 result.
pub type Result<T> = core::result::Result<T, Error>;

#[panic_handler]
#[cfg(not(feature = "std"))]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { libc::abort() }
}

struct LibcAllocator;

unsafe impl GlobalAlloc for LibcAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        libc::aligned_malloc(layout.size(), layout.align()).cast()
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        libc::aligned_free(ptr.cast())
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        use libc::{c_void, size_t};

        extern "C" {
            #[link_name = "_aligned_realloc"]
            pub fn aligned_realloc(
                ptr: *mut c_void,
                size: size_t,
                alignment: size_t,
            ) -> *mut c_void;
        }

        aligned_realloc(ptr.cast(), new_size, layout.align()).cast()
    }
}

#[global_allocator]
static ALLOCATOR: LibcAllocator = LibcAllocator;
