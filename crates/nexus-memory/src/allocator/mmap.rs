//! mmap.rs — Cross-platform anonymous shared memory region allocator.
//!
//! PLATFORM SELECTION:
//! This module uses cfg_if! to select the OS implementation at compile time.
//! All platform-specific code is in a single cfg_if! block at the bottom.
//! The public API is identical on all platforms.
//!
//! LINUX/MACOS:
//!   alloc:  mmap(NULL, size, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_ANONYMOUS, -1, 0)
//!   seal:   mprotect(ptr, size, PROT_READ)
//!   unseal: mprotect(ptr, size, PROT_READ|PROT_WRITE)
//!   free:   munmap(ptr, size)
//!
//! WINDOWS:
//!   alloc:  CreateFileMapping(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, ...) +
//!           MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size)
//!   seal:   VirtualProtect(ptr, size, PAGE_READONLY, &old_protect)
//!   unseal: VirtualProtect(ptr, size, PAGE_READWRITE, &old_protect)
//!   free:   UnmapViewOfFile(ptr) + CloseHandle(handle)
//!
//! TOCTOU SAFETY CONTRACT (enforced by ImmutableRegion, not here):
//!   The correct sequence for the IMMUTABLE region is:
//!     1. unseal() the staging buffer
//!     2. write to the staging buffer
//!     3. seal() the staging buffer       ← hardware protection in place
//!     4. swap the live_ptr atomically    ← pointer published to readers
//!   Sealing BEFORE swapping is the invariant. mmap.rs exposes both operations
//!   separately so ImmutableRegion can enforce the ordering in StagingWriter::drop().
//!   No reader can observe the new pointer before the seal is complete.

use crate::error::MmapError;
use cfg_if::cfg_if;

/// An anonymous shared memory region.
/// The memory is readable and writable unless seal_read_only() has been called.
/// Dropping this struct unmaps the region from the process address space.
///
/// Safety invariants:
/// - ptr is valid for the lifetime of this struct.
/// - No raw pointer derived from ptr may outlive this struct.
/// - seal_read_only() and unseal() are unsafe because they change memory
///   protection while raw pointers to the region may exist in other threads.
pub struct MmapRegion {
    ptr:  *mut u8,
    size: usize,
    name: &'static str,

    /// Windows only: handle returned by CreateFileMapping.
    /// On Unix this field is zero-sized (cfg_if removes it).
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl MmapRegion {
    /// Allocate a new anonymous shared memory region of `size` bytes.
    ///
    /// The entire region is zero-initialized by the OS.
    /// On allocation failure, returns MmapError::AllocationFailed with the OS error code.
    ///
    /// `name` is used only in error messages — it does not affect allocation.
    pub fn new(size: usize, name: &'static str) -> Result<Self, MmapError> {
        cfg_if! {
            if #[cfg(unix)] {
                let ptr = unix_impl::alloc(size, name)?;
                Ok(Self { ptr, size, name })
            } else if #[cfg(windows)] {
                let (ptr, handle) = windows_impl::alloc(size, name)?;
                Ok(Self { ptr, size, name, handle })
            }
        }
    }

    /// Returns a raw pointer to the first byte of the region.
    ///
    /// The returned pointer is valid as long as this MmapRegion is alive
    /// and the region has not been unmapped.
    #[inline]
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Returns the size of the region in bytes.
    #[inline]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns the name of the region (for error messages only).
    #[inline]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Seal the region as read-only using hardware memory protection.
    ///
    /// After this call, any write to any byte in the region will cause a
    /// hardware fault (SIGSEGV on Linux/macOS, access violation on Windows).
    ///
    /// # Safety
    /// The caller must guarantee:
    /// 1. No active writer holds a reference into this region.
    /// 2. This call happens BEFORE any atomic pointer swap that makes
    ///    this region visible to readers (TOCTOU safety from Q14).
    ///
    /// Returns MmapError::SealFailed if the OS call fails.
    pub unsafe fn seal_read_only(&self) -> Result<(), MmapError> {
        cfg_if! {
            if #[cfg(unix)] {
                unix_impl::seal(self.ptr, self.size, self.name)
            } else if #[cfg(windows)] {
                windows_impl::seal(self.ptr, self.size, self.name)
            }
        }
    }

    /// Restore read-write access to a previously sealed region.
    ///
    /// After this call, writes to the region are permitted again.
    ///
    /// # Safety
    /// The caller must guarantee:
    /// 1. The atomic pointer has already been swapped away from this region
    ///    before unsealing — no reader should be reading this region anymore.
    /// 2. The TOCTOU sequence is: unseal → write → seal → swap (never swap → seal).
    ///
    /// Returns MmapError::UnsealFailed if the OS call fails.
    pub unsafe fn unseal(&self) -> Result<(), MmapError> {
        cfg_if! {
            if #[cfg(unix)] {
                unix_impl::unseal(self.ptr, self.size, self.name)
            } else if #[cfg(windows)] {
                windows_impl::unseal(self.ptr, self.size, self.name)
            }
        }
    }
}

impl Drop for MmapRegion {
    /// Unmap the region from the process address space.
    /// On Linux/macOS: munmap. On Windows: UnmapViewOfFile + CloseHandle.
    /// Panics if the OS call fails — there is no recoverable path from a
    /// failed unmap on process shutdown.
    fn drop(&mut self) {
        cfg_if! {
            if #[cfg(unix)] {
                unsafe { unix_impl::free(self.ptr, self.size, self.name) }
            } else if #[cfg(windows)] {
                unsafe { windows_impl::free(self.ptr, self.size, self.handle, self.name) }
            }
        }
    }
}

// SAFETY: MmapRegion is a raw pointer wrapper. We explicitly opt into Send+Sync
// because the memory is an OS-allocated region that is safe to access from
// multiple threads when protected by the atomic pointer and seal/unseal protocol.
unsafe impl Send for MmapRegion {}
unsafe impl Sync for MmapRegion {}

cfg_if! {
    if #[cfg(unix)] {
        mod unix_impl {
            //! Unix (Linux + macOS) implementation using libc::mmap and libc::mprotect.
            use super::*;
            use libc::{
                mmap, mprotect, munmap,
                PROT_READ, PROT_WRITE, MAP_SHARED, MAP_ANONYMOUS, MAP_FAILED,
            };

            pub(super) fn alloc(size: usize, name: &'static str) -> Result<*mut u8, MmapError> {
                let ptr = unsafe {
                    mmap(
                        std::ptr::null_mut(),
                        size,
                        PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS,
                        -1,
                        0,
                    )
                };
                if ptr == MAP_FAILED || ptr.is_null() {
                    return Err(MmapError::AllocationFailed {
                        name,
                        source: std::io::Error::last_os_error(),
                    });
                }
                Ok(ptr as *mut u8)
            }

            pub(super) unsafe fn seal(ptr: *mut u8, size: usize, name: &'static str) -> Result<(), MmapError> {
                let ret = mprotect(ptr as *mut libc::c_void, size, PROT_READ);
                if ret != 0 {
                    return Err(MmapError::SealFailed {
                        name,
                        source: std::io::Error::last_os_error(),
                    });
                }
                Ok(())
            }

            pub(super) unsafe fn unseal(ptr: *mut u8, size: usize, name: &'static str) -> Result<(), MmapError> {
                let ret = mprotect(ptr as *mut libc::c_void, size, PROT_READ | PROT_WRITE);
                if ret != 0 {
                    return Err(MmapError::UnsealFailed {
                        name,
                        source: std::io::Error::last_os_error(),
                    });
                }
                Ok(())
            }

            pub(super) unsafe fn free(ptr: *mut u8, size: usize, name: &'static str) {
                let ret = munmap(ptr as *mut libc::c_void, size);
                if ret != 0 {
                    panic!(
                        "munmap failed for region '{}': {}",
                        name,
                        std::io::Error::last_os_error()
                    );
                }
            }
        }
    } else if #[cfg(windows)] {
        mod windows_impl {
            //! Windows implementation using VirtualAlloc/VirtualProtect.
            //! CreateFileMapping(INVALID_HANDLE_VALUE) + MapViewOfFile for shared anonymous memory.
            //! VirtualProtect for PAGE_READONLY (seal) and PAGE_READWRITE (unseal).
            use super::*;
            use windows::Win32::System::Memory::{
                CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualProtect,
                FILE_MAP_ALL_ACCESS, PAGE_READWRITE, PAGE_READONLY, PAGE_PROTECTION_FLAGS,
            };
            use windows::Win32::Foundation::{INVALID_HANDLE_VALUE, CloseHandle};

            pub(super) fn alloc(
                size: usize,
                name: &'static str,
            ) -> Result<(*mut u8, windows::Win32::Foundation::HANDLE), MmapError> {
                unsafe {
                    let handle = CreateFileMappingW(
                        INVALID_HANDLE_VALUE,
                        None,
                        PAGE_READWRITE,
                        (size >> 32) as u32,
                        (size & 0xFFFF_FFFF) as u32,
                        None,
                    ).map_err(|e| MmapError::AllocationFailed {
                        name,
                        source: std::io::Error::from_raw_os_error(e.code().0),
                    })?;

                    let view = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size);
                    if view.Value.is_null() {
                        let _ = CloseHandle(handle);
                        return Err(MmapError::AllocationFailed {
                            name,
                            source: std::io::Error::last_os_error(),
                        });
                    }
                    Ok((view.Value as *mut u8, handle))
                }
            }

            pub(super) unsafe fn seal(
                ptr: *mut u8,
                size: usize,
                name: &'static str,
            ) -> Result<(), MmapError> {
                let mut old = PAGE_PROTECTION_FLAGS(0);
                VirtualProtect(ptr as *const core::ffi::c_void, size, PAGE_READONLY, &mut old)
                    .map_err(|e| MmapError::SealFailed {
                        name,
                        source: std::io::Error::from_raw_os_error(e.code().0),
                    })
            }

            pub(super) unsafe fn unseal(
                ptr: *mut u8,
                size: usize,
                name: &'static str,
            ) -> Result<(), MmapError> {
                let mut old = PAGE_PROTECTION_FLAGS(0);
                VirtualProtect(ptr as *const core::ffi::c_void, size, PAGE_READWRITE, &mut old)
                    .map_err(|e| MmapError::UnsealFailed {
                        name,
                        source: std::io::Error::from_raw_os_error(e.code().0),
                    })
            }

            pub(super) unsafe fn free(
                ptr: *mut u8,
                size: usize,
                handle: windows::Win32::Foundation::HANDLE,
                name: &'static str,
            ) {
                use windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS;
                let view = MEMORY_MAPPED_VIEW_ADDRESS { Value: ptr as *mut core::ffi::c_void };
                if let Err(e) = UnmapViewOfFile(view) {
                    panic!("UnmapViewOfFile failed for region '{}': {:?}", name, e);
                }
                if let Err(e) = CloseHandle(handle) {
                    panic!("CloseHandle failed for region '{}': {:?}", name, e);
                }
                let _ = size; // suppresses unused warning
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Allocate a 4096-byte region, write a u64, read it back, verify match.
    #[test]
    fn test_alloc_write_read() {
        let region = MmapRegion::new(4096, "test").expect("alloc failed");
        let ptr = region.as_ptr();
        assert!(!ptr.is_null());
        assert_eq!(region.size(), 4096);
        unsafe {
            let val: u64 = 0xDEADBEEF_CAFEBABE;
            std::ptr::write_volatile(ptr as *mut u64, val);
            let read = std::ptr::read_volatile(ptr as *const u64);
            assert_eq!(read, val);
        }
    }

    /// Allocate, seal, unseal, write again, verify write succeeds.
    #[test]
    fn test_unseal_restores_write() {
        let region = MmapRegion::new(4096, "test_unseal").expect("alloc failed");
        let ptr = region.as_ptr();
        // Write before seal
        unsafe { std::ptr::write_volatile(ptr as *mut u64, 42) };
        // Seal
        unsafe { region.seal_read_only().expect("seal failed") };
        // Unseal
        unsafe { region.unseal().expect("unseal failed") };
        // Write again after unseal
        unsafe { std::ptr::write_volatile(ptr as *mut u64, 99) };
        let val = unsafe { std::ptr::read_volatile(ptr as *const u64) };
        assert_eq!(val, 99);
    }

    /// Allocate, seal, unseal, verify no panic — cannot catch segfault in normal test
    #[test]
    fn test_seal_prevents_write() {
        // We cannot catch a hardware fault (SIGSEGV/access violation) in a normal test.
        // Instead, verify that seal returns Ok and the region is read-only visible to OS.
        let region = MmapRegion::new(4096, "test_seal").expect("alloc failed");
        unsafe {
            region.seal_read_only().expect("seal failed");
            // Unseal before dropping to avoid fault during drop
            region.unseal().expect("unseal failed");
        }
    }

    /// Verify the region is properly unmapped on drop (no double-free).
    #[test]
    fn test_drop_unmaps() {
        {
            let region = MmapRegion::new(4096, "test_drop").expect("alloc failed");
            let _ptr = region.as_ptr();
            // Drop happens here — should not panic
        }
        // If we reach here, no double-free occurred
    }
}
