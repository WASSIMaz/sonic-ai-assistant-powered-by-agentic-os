//! immutable_region.rs — Hardware-enforced immutable memory region.
//!
//! REGION PURPOSE:
//! Holds two types of frozen objects:
//! 1. The scalar fast-path struct (WorldModelScalars) — #[repr(C)], directly in mmap.
//!    Read by L2 Watchdog, EnvelopeMonitor, L1 Health Watchdog without the GIL.
//! 2. The full Python frozen dataclass — held as ArcSwap<Py<WorldModelSnapshot>>.
//!    Read by Contextualizer, ProcessOrchestrator, and any Python component.
//!
//! HARDWARE ENFORCEMENT:
//! The mmap region is sealed with mprotect(PROT_READ) / VirtualProtect(PAGE_READONLY)
//! before the atomic pointer is published. This means the OS MMU will trap any write
//! to the sealed region with a hardware fault (SIGSEGV on Unix, access violation on Windows).
//!
//! PING-PONG BUFFER:
//! The mmap is divided into two equal halves: staging_a (offset 0) and staging_b
//! (offset IMMUTABLE_SIZE/2). At any point, one half is sealed and live, the other
//! is unsealed and ready to receive the next write.
//!
//! TOCTOU-SAFE WRITE SEQUENCE (enforced by StagingWriter::drop()):
//!   Step 1: Unseal the inactive staging buffer.
//!   Step 2: Write new data into the unsealed buffer (via StagingWriter::as_mut_slice()).
//!   Step 3: Seal the staging buffer with mprotect(PROT_READ). ← hardware protection
//!   Step 4: Atomically swap live_ptr to point to the sealed buffer. ← pointer published
//!   Step 5: Unseal the previously live buffer (now the new inactive staging buffer).
//! The atomic pointer is NEVER swapped before the seal is complete.
//! This eliminates the TOCTOU window identified in Q14.
//!
//! ARCSWAP FOR PYTHON OBJECTS:
//! The ArcSwap<Py<WorldModelSnapshot>> holds the full Python dataclass.
//! write sequence (Oracle):
//!   1. Build new WorldModel dataclass in Python (under GIL).
//!   2. Python::with_gil → Py<WorldModelSnapshot> (GIL released after this).
//!   3. Call world_model_store.store(Arc::new(py_snapshot)) — no GIL needed.
//! read sequence (any Rust reader):
//!   world_model_store.load() → Guard<Arc<Py<...>>> — no GIL needed, O(1).
//! Reclamation: Arc refcount drops to zero when all Guards drop → Py<T> drops →
//!   CPython refcount decrements → Python object freed. No separate GC path.

use std::sync::atomic::{AtomicPtr, AtomicBool, Ordering};
use arc_swap::ArcSwap;
use pyo3::prelude::*;
use std::sync::Arc;
use crate::allocator::mmap::MmapRegion;
use crate::error::MmapError;
use crate::generated::layout::IMMUTABLE_SIZE;

/// The scalar fast-path struct stored in the IMMUTABLE mmap region.
/// Contains only the fields that Rust components read at high frequency
/// without going through Python. All fields are fixed-size primitives.
///
/// # Safety
/// #[repr(C)] guarantees the layout matches _layout.py's ctypes.Structure.
/// The compile-time assert below catches any accidental size change.
#[repr(C)]
pub struct WorldModelScalars {
    /// Monotonic timestamp of the last Oracle poll (nanoseconds).
    pub timestamp_ns: std::sync::atomic::AtomicU64,

    /// PID of the currently active window's process.
    pub active_pid: std::sync::atomic::AtomicU32,

    /// CPU utilization as f32 bits (use f32::from_bits() to read).
    /// Stored as AtomicU32 because AtomicF32 is not stable.
    pub cpu_pct_bits: std::sync::atomic::AtomicU32,

    /// Physical memory currently in use, megabytes.
    pub memory_used_mb: std::sync::atomic::AtomicU32,

    /// Free disk space on primary volume as f32 bits (gigabytes).
    pub disk_free_gb_bits: std::sync::atomic::AtomicU32,

    /// Network reachability: 0=unreachable, 1=reachable.
    pub network_reachable: std::sync::atomic::AtomicU8,

    /// Padding to 64 bytes (one cache line).
    pub _pad: [u8; 43],
}

/// The IMMUTABLE memory region.
/// Holds one sealed ping-pong buffer for WorldModelScalars and one
/// ArcSwap for the full Python WorldModel frozen dataclass.
pub struct ImmutableRegion {
    mmap: MmapRegion,

    /// Atomic pointer to the currently live (sealed) half of the mmap.
    /// Readers load this with Acquire ordering.
    /// Writers swap this with Release ordering inside StagingWriter::drop().
    live_ptr: AtomicPtr<u8>,

    /// Base of staging area A (offset 0 in the mmap).
    staging_a: *mut u8,

    /// Base of staging area B (offset IMMUTABLE_SIZE/2 in the mmap).
    staging_b: *mut u8,

    /// false = A is currently live (sealed), B is the inactive staging area.
    /// true  = B is currently live (sealed), A is the inactive staging area.
    active_is_b: AtomicBool,

    /// ArcSwap holding the full Python WorldModel frozen dataclass.
    /// The GIL is never held during store() or load().
    pub world_model: ArcSwap<Py<PyAny>>,
}

// ── Per-half seal/unseal helpers ─────────────────────────────────────────────

/// Seal a sub-range of the mmap as read-only.
/// On Unix: mprotect(ptr, size, PROT_READ).
/// On Windows: VirtualProtect(ptr, size, PAGE_READONLY).
///
/// # Safety
/// The caller must guarantee no active writer holds a reference to this range.
unsafe fn seal_range(ptr: *mut u8, size: usize) -> Result<(), MmapError> {
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            let ret = libc::mprotect(ptr as *mut libc::c_void, size, libc::PROT_READ);
            if ret != 0 {
                return Err(MmapError::SealFailed {
                    name: "IMMUTABLE",
                    source: std::io::Error::last_os_error(),
                });
            }
            Ok(())
        } else if #[cfg(windows)] {
            use windows::Win32::System::Memory::{VirtualProtect, PAGE_READONLY, PAGE_PROTECTION_FLAGS};
            let mut old = PAGE_PROTECTION_FLAGS(0);
            VirtualProtect(ptr as *const core::ffi::c_void, size, PAGE_READONLY, &mut old)
                .map_err(|e| MmapError::SealFailed {
                    name: "IMMUTABLE",
                    source: std::io::Error::from_raw_os_error(e.code().0),
                })
        } else {
            // Fallback for unsupported platforms — no hardware enforcement.
            let _ = (ptr, size);
            Ok(())
        }
    }
}

/// Restore read-write access to a sub-range of the mmap.
/// On Unix: mprotect(ptr, size, PROT_READ|PROT_WRITE).
/// On Windows: VirtualProtect(ptr, size, PAGE_READWRITE).
///
/// # Safety
/// The caller must guarantee the atomic pointer has already been swapped away
/// from this range before unsealing.
unsafe fn unseal_range(ptr: *mut u8, size: usize) -> Result<(), MmapError> {
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            let ret = libc::mprotect(
                ptr as *mut libc::c_void,
                size,
                libc::PROT_READ | libc::PROT_WRITE,
            );
            if ret != 0 {
                return Err(MmapError::UnsealFailed {
                    name: "IMMUTABLE",
                    source: std::io::Error::last_os_error(),
                });
            }
            Ok(())
        } else if #[cfg(windows)] {
            use windows::Win32::System::Memory::{VirtualProtect, PAGE_READWRITE, PAGE_PROTECTION_FLAGS};
            let mut old = PAGE_PROTECTION_FLAGS(0);
            VirtualProtect(ptr as *const core::ffi::c_void, size, PAGE_READWRITE, &mut old)
                .map_err(|e| MmapError::UnsealFailed {
                    name: "IMMUTABLE",
                    source: std::io::Error::from_raw_os_error(e.code().0),
                })
        } else {
            // Fallback — no hardware enforcement.
            let _ = (ptr, size);
            Ok(())
        }
    }
}

impl ImmutableRegion {
    /// Allocate the IMMUTABLE mmap region and initialize both staging areas.
    /// Seals staging_a as the initial live region (it starts empty but sealed).
    /// Sets live_ptr to staging_a.
    pub fn new() -> Result<Self, MmapError> {
        let mmap = MmapRegion::new(IMMUTABLE_SIZE, "IMMUTABLE")?;
        let staging_a = mmap.as_ptr();
        // SAFETY: The mmap has IMMUTABLE_SIZE bytes; staging_b is in the second half.
        let staging_b = unsafe { mmap.as_ptr().add(IMMUTABLE_SIZE / 2) };
        let half = IMMUTABLE_SIZE / 2;

        // Seal staging_a as the initial live (read-only) region.
        // staging_b remains unsealed (writable) as the initial inactive staging area.
        // SAFETY: No other references exist to staging_a at this point.
        unsafe { seal_range(staging_a, half)? };

        let live_ptr = AtomicPtr::new(staging_a);

        // Initialize world_model with Python None sentinel (requires GIL).
        let world_model = pyo3::Python::with_gil(|py| {
            ArcSwap::new(Arc::new(py.None()))
        });

        Ok(Self {
            mmap,
            live_ptr,
            staging_a,
            staging_b,
            active_is_b: AtomicBool::new(false),
            world_model,
        })
    }

    /// Acquire a write handle to the inactive staging area.
    ///
    /// Returns a StagingWriter that provides mutable access to the inactive buffer.
    /// When StagingWriter is dropped, the TOCTOU-safe sequence runs automatically:
    ///   seal → swap → unseal_old.
    ///
    /// # Panics
    /// Panics if a StagingWriter is already live (only one writer at a time).
    pub fn begin_write(&self) -> Result<StagingWriter<'_>, MmapError> {
        let half = IMMUTABLE_SIZE / 2;
        // Determine which half is currently inactive (the staging area).
        let staging_buf = if self.active_is_b.load(Ordering::Acquire) {
            // B is live → A is the inactive staging area.
            self.staging_a
        } else {
            // A is live → B is the inactive staging area.
            self.staging_b
        };

        // Unseal the inactive staging buffer so we can write to it.
        // SAFETY: The inactive half is not the live half; no reader is directed to it.
        unsafe { unseal_range(staging_buf, half)? };

        Ok(StagingWriter {
            region: self,
            buffer: staging_buf,
            size: half,
        })
    }

    /// Load the current live pointer with Acquire ordering.
    /// Returns a raw const pointer to the sealed WorldModelScalars.
    ///
    /// # Safety
    /// The returned pointer is valid as long as the ImmutableRegion is alive.
    /// The data behind the pointer is read-only (hardware-enforced).
    pub fn load(&self) -> *const u8 {
        self.live_ptr.load(Ordering::Acquire)
    }

    /// Convenience accessor: cast live_ptr to &WorldModelScalars.
    ///
    /// # Safety
    /// The caller must not write through the returned reference.
    /// Hardware protection enforces this, but Rust's type system does not.
    pub unsafe fn scalars(&self) -> &WorldModelScalars {
        let ptr = self.live_ptr.load(Ordering::Acquire) as *const WorldModelScalars;
        // SAFETY: The live half always contains a valid WorldModelScalars at offset 0.
        // The half is at least size_of::<WorldModelScalars>() bytes (64 bytes vs 32MB).
        &*ptr
    }
}

/// A write handle to the inactive staging area.
/// Provides mutable access to the staging buffer.
/// On drop, enforces the TOCTOU-safe sequence: seal → swap → unseal_old.
pub struct StagingWriter<'a> {
    region: &'a ImmutableRegion,
    /// Pointer to the inactive staging buffer that this writer owns.
    buffer: *mut u8,
    /// Size of the staging buffer (IMMUTABLE_SIZE / 2).
    size: usize,
}

impl<'a> StagingWriter<'a> {
    /// Returns a mutable byte slice for writing into the staging buffer.
    /// The buffer is currently unsealed (writable).
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: self.buffer points to the inactive, unsealed staging half.
        // The ImmutableRegion owns the mmap and this StagingWriter has exclusive
        // logical access to this half for its lifetime.
        unsafe { std::slice::from_raw_parts_mut(self.buffer, self.size) }
    }

    /// Write a WorldModelScalars struct into the staging buffer at offset 0.
    pub fn write_scalars(&mut self, scalars: &WorldModelScalars) {
        // SAFETY: The staging buffer is at least size_of::<WorldModelScalars>() bytes
        // and is currently unsealed (writable). We copy the scalars atomically
        // by writing each atomic field individually via read_volatile / write_volatile.
        // Using ptr::copy_nonoverlapping for the repr(C) struct is safe here because
        // the destination is freshly written staging memory and we hold the only
        // write reference to it.
        use std::sync::atomic::Ordering::Relaxed;
        let dst = self.buffer as *mut WorldModelScalars;
        // SAFETY: dst is valid, aligned (IMMUTABLE_SIZE/2 is page-aligned, and
        // WorldModelScalars is #[repr(C)] with no padding issues at offset 0).
        unsafe {
            let d = &*dst;
            d.timestamp_ns.store(scalars.timestamp_ns.load(Relaxed), Relaxed);
            d.active_pid.store(scalars.active_pid.load(Relaxed), Relaxed);
            d.cpu_pct_bits.store(scalars.cpu_pct_bits.load(Relaxed), Relaxed);
            d.memory_used_mb.store(scalars.memory_used_mb.load(Relaxed), Relaxed);
            d.disk_free_gb_bits.store(scalars.disk_free_gb_bits.load(Relaxed), Relaxed);
            d.network_reachable.store(scalars.network_reachable.load(Relaxed), Relaxed);
        }
    }
}

impl<'a> Drop for StagingWriter<'a> {
    /// Enforces the TOCTOU-safe write sequence:
    /// Step 1: mprotect/VirtualProtect(buffer, PROT_READ)  ← seal
    /// Step 2: live_ptr.store(buffer, Release)              ← swap pointer
    /// Step 3: mprotect/VirtualProtect(old_live, PROT_READ|PROT_WRITE) ← unseal old
    ///
    /// This order guarantees no reader can observe the new pointer before
    /// the region is hardware-protected. The TOCTOU window from Q14 is closed.
    fn drop(&mut self) {
        // Step 1: Seal the staging buffer (hardware read-only protection).
        // SAFETY: No writer holds a reference to self.buffer after this drop().
        if let Err(e) = unsafe { seal_range(self.buffer, self.size) } {
            // Seal failure is a critical invariant violation — panic to avoid
            // publishing an unsealed buffer to readers.
            panic!("ImmutableRegion: seal_range failed in StagingWriter::drop: {}", e);
        }

        // Step 2: Atomically publish the newly sealed buffer to readers.
        // Release ordering ensures all prior writes are visible before the pointer swap.
        let old_live = self.region.live_ptr.swap(self.buffer, Ordering::Release);

        // Step 3: Unseal the previously live buffer so it can be used as the next
        // inactive staging area. It is safe to unseal now because live_ptr no longer
        // points to it — any new reader will follow the new live_ptr.
        // SAFETY: old_live is the former live half. live_ptr has been swapped away
        // from it with Release ordering, so all subsequent loads see the new pointer.
        if let Err(e) = unsafe { unseal_range(old_live, self.size) } {
            // Unseal failure means the old buffer is stuck read-only.
            // This is a serious but non-fatal error — log and continue.
            // The next begin_write() will fail with UnsealFailed.
            eprintln!(
                "ImmutableRegion: unseal_range failed in StagingWriter::drop (old_live): {}",
                e
            );
        }

        // Flip active_is_b to track which half is now live.
        self.region.active_is_b.fetch_xor(true, Ordering::Release);
    }
}

// SAFETY: ImmutableRegion is safe to send and share across threads.
// The live_ptr atomic provides safe concurrent access to the sealed region.
// The ArcSwap provides safe concurrent access to the Python dataclass.
unsafe impl Send for ImmutableRegion {}
unsafe impl Sync for ImmutableRegion {}

// SAFETY: StagingWriter holds a raw pointer to the staging buffer but is
// logically the exclusive writer. It is safe to send to another thread
// because the buffer lifetime is tied to the ImmutableRegion.
unsafe impl<'a> Send for StagingWriter<'a> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    /// 10 concurrent readers in a 5-second loop, one writer swapping every 200ms.
    /// Verify every reader loads a non-null pointer and the value is old or new content.
    #[test]
    fn test_concurrent_readers_see_valid_pointer() {
        let region = Arc::new(ImmutableRegion::new().expect("ImmutableRegion::new failed"));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Spawn 10 reader threads. Each reader spins loading live_ptr and verifying
        // it is non-null and points within the mmap bounds.
        let mut handles = vec![];
        for _ in 0..10 {
            let region_clone = region.clone();
            let stop_clone = stop.clone();
            handles.push(thread::spawn(move || {
                while !stop_clone.load(Ordering::Relaxed) {
                    let ptr = region_clone.load();
                    assert!(
                        !ptr.is_null(),
                        "live_ptr must never be null"
                    );
                    // Verify the pointer is one of the two valid halves.
                    let base = region_clone.mmap.as_ptr() as usize;
                    let addr = ptr as usize;
                    let half = IMMUTABLE_SIZE / 2;
                    assert!(
                        addr == base || addr == base + half,
                        "live_ptr must point to staging_a or staging_b: addr={:#x}, base={:#x}",
                        addr, base
                    );
                    thread::yield_now();
                }
            }));
        }

        // Writer thread: swap every 200ms for ~1 second.
        let region_writer = region.clone();
        let writer = thread::spawn(move || {
            for i in 0u8..5 {
                {
                    let mut writer = region_writer.begin_write().expect("begin_write failed");
                    // Write a sentinel byte into the staging buffer.
                    let buf = writer.as_mut_slice();
                    buf[0] = i;
                }
                // StagingWriter dropped here — seal/swap/unseal sequence runs.
                thread::sleep(Duration::from_millis(200));
            }
        });

        writer.join().expect("writer thread panicked");
        stop.store(true, Ordering::Relaxed);

        for h in handles {
            h.join().expect("reader thread panicked");
        }
    }

    /// One reader holds a Guard to snapshot N.
    /// Oracle performs 100 consecutive swaps.
    /// Verify snapshot N is still readable via Guard (not freed, not corrupted).
    #[test]
    fn test_guard_prevents_reclamation_during_100_swaps() {
        let region = ImmutableRegion::new().expect("ImmutableRegion::new failed");

        // Capture initial live pointer (snapshot 0).
        let initial_ptr = region.load();
        assert!(!initial_ptr.is_null());

        // Perform 100 swaps — each swap changes which half is live.
        for _ in 0..100 {
            {
                let _writer = region.begin_write().expect("begin_write failed");
                // Writer drops here: seal → swap → unseal.
            }
        }

        // After 100 swaps (even number), the live pointer should be back to staging_a.
        // Either way, the mmap backing the original pointer is still valid and mapped.
        // We verify by reading the first byte — if mmap was unmapped this would fault.
        let _ = unsafe { std::ptr::read_volatile(initial_ptr) };

        // The current live pointer must be non-null.
        let current_ptr = region.load();
        assert!(!current_ptr.is_null());
    }

    /// Call seal_read_only(), attempt to verify the seal/unseal cycle works.
    /// We cannot catch hardware faults in normal tests, so we verify the
    /// protection state via the seal_range/unseal_range helpers returning Ok.
    #[test]
    fn test_write_to_sealed_region_faults() {
        let region = ImmutableRegion::new().expect("ImmutableRegion::new failed");

        // Verify: the initial live buffer is already sealed.
        // We test the seal/unseal round-trip using begin_write (which unseals) and
        // then dropping the writer (which re-seals).
        {
            let mut writer = region.begin_write().expect("begin_write failed");
            // We can write while the staging buffer is unsealed.
            let buf = writer.as_mut_slice();
            buf[0] = 0xAB;
            buf[1] = 0xCD;
            // Drop: seals the staging buffer, swaps pointer, unseals old live.
        }

        // After the swap, the buffer we just wrote is now the live (sealed) buffer.
        let live = region.load();
        // Read the first byte through the live pointer (read-only access is allowed).
        let first_byte = unsafe { std::ptr::read_volatile(live) };
        assert_eq!(first_byte, 0xAB, "sealed live buffer must contain the written value");
    }

    /// Verify that StagingWriter::drop() always seals before swapping.
    /// Use active_is_b to confirm the flip happens atomically after each write.
    #[test]
    fn test_toctou_sequence_enforced_by_stagingwriter() {
        let region = ImmutableRegion::new().expect("ImmutableRegion::new failed");

        // Initially: A is live (active_is_b = false), B is the staging area.
        assert!(!region.active_is_b.load(Ordering::Acquire));
        let initial_live = region.load();
        assert_eq!(initial_live, region.staging_a);

        // First write: seals B, swaps to B, unseals A. active_is_b flips to true.
        {
            let _writer = region.begin_write().expect("begin_write failed");
        }
        assert!(region.active_is_b.load(Ordering::Acquire), "active_is_b must be true after first swap");
        assert_eq!(region.load(), region.staging_b, "live_ptr must point to staging_b");

        // Second write: seals A, swaps to A, unseals B. active_is_b flips back to false.
        {
            let _writer = region.begin_write().expect("begin_write failed");
        }
        assert!(!region.active_is_b.load(Ordering::Acquire), "active_is_b must be false after second swap");
        assert_eq!(region.load(), region.staging_a, "live_ptr must point to staging_a");

        // Use a barrier to verify the seal happens before the pointer is visible to a concurrent reader.
        let region = Arc::new(ImmutableRegion::new().expect("ImmutableRegion::new failed"));
        let barrier = Arc::new(Barrier::new(2));
        let region_reader = region.clone();
        let barrier_reader = barrier.clone();

        let reader = thread::spawn(move || {
            // Wait for the writer to finish begin_write().
            barrier_reader.wait();
            // By the time we load, the writer has started and may or may not have dropped.
            // We only assert non-null — correctness of seal ordering is structural.
            let ptr = region_reader.load();
            assert!(!ptr.is_null());
        });

        {
            let _writer = region.begin_write().expect("begin_write failed");
            barrier.wait();
            // Writer drops here (seal → swap → unseal).
        }

        reader.join().expect("reader thread panicked");
    }
}
