//! slab.rs — Static pre-allocation slab allocator for typed slots.
//!
//! DESIGN: Static pre-allocation. All SLOT_COUNT slots are allocated at startup
//! within the provided mmap region. Slots are never freed at the OS level.
//! A lock-free free-list (atomic stack) tracks which slots are available.
//!
//! FREE-LIST PROTOCOL:
//! The free-list is an atomic stack implemented as:
//!   - free_head: AtomicUsize — index of the top of the stack, SENTINEL if empty
//!   - free_next: [AtomicUsize; SLOT_COUNT] — next pointer for each slot
//! Initial state: free_head = 0, free_next[i] = i+1, free_next[N-1] = SENTINEL
//!
//! acquire() pops the head with compare_exchange_weak loop.
//! release(index) pushes index onto the head with compare_exchange_weak loop.
//! Both operations are wait-free in the common case (no contention) and
//! lock-free under contention (finite retries to global progress).
//!
//! SLOT POINTER ARITHMETIC:
//! slot_pointer(index) = base + index * SLOT_SIZE
//! Verified at test time: slot_pointer - base == index * SLOT_SIZE for all slots.
//!
//! EXHAUSTION HANDLING:
//! acquire() returns None when free_head == SENTINEL.
//! The caller (goal acceptance gate) must publish RESOURCE_EXHAUSTED to the Bus
//! and refuse the goal BEFORE calling acquire(). The slab itself never panics.
//!
//! SLAB CLASSES (from Q5):
//! - SlabA<3072, 256>: GoalContract slots
//! - SlabB<1024, 512>: GoalState/SubGoalNode slots
//! - SlabC<320,  150>: ObservationSnapshot slots
//! - SlabD<8,   1024>: AtomicU64 counter slots

use std::sync::atomic::{AtomicUsize, Ordering};

/// Sentinel value indicating an empty free-list.
const SENTINEL: usize = usize::MAX;

/// A statically-sized slab allocator for fixed-size typed slots.
///
/// SLOT_SIZE: size of each slot in bytes (must be > 0).
/// SLOT_COUNT: maximum number of slots (fixed at compile time from nexus_layout.toml).
///
/// The slab does not own its memory — it borrows a region from an MmapRegion.
/// The MmapRegion must outlive the SlabAllocator.
pub struct SlabAllocator<const SLOT_SIZE: usize, const SLOT_COUNT: usize> {
    /// Pointer to the first byte of the first slot (within an MmapRegion).
    base: *mut u8,

    /// Index of the top of the free-list stack. SENTINEL when empty.
    free_head: AtomicUsize,

    /// free_next[i] = index of the next free slot after slot i.
    /// Initialized as: free_next[i] = i+1, free_next[SLOT_COUNT-1] = SENTINEL.
    free_next: [AtomicUsize; SLOT_COUNT],

    /// Monotonic count of currently allocated (in-use) slots.
    /// Incremented by acquire(), decremented by release().
    /// Used for exhaustion detection and the slab exhaustion test.
    allocated: AtomicUsize,
}

impl<const SLOT_SIZE: usize, const SLOT_COUNT: usize>
    SlabAllocator<SLOT_SIZE, SLOT_COUNT>
{
    /// Initialize the slab allocator from a raw pointer into an MmapRegion.
    ///
    /// Builds the initial free-list: 0 → 1 → 2 → ... → SLOT_COUNT-1 → SENTINEL.
    /// Zeroes all slot memory (OS already zero-initialized mmap, this is a safety guarantee).
    ///
    /// # Safety
    /// - `base` must point to at least SLOT_SIZE * SLOT_COUNT bytes of writable
    ///   memory within an MmapRegion that outlives this SlabAllocator.
    /// - Must be called exactly once per region — calling twice causes aliasing.
    pub unsafe fn init(base: *mut u8) -> Self {
        // Zero-initialize all slot memory
        std::ptr::write_bytes(base, 0, SLOT_SIZE * SLOT_COUNT);

        // Build initial free list: free_next[i] = i+1, free_next[N-1] = SENTINEL
        let free_next = std::array::from_fn(|i| {
            AtomicUsize::new(if i + 1 < SLOT_COUNT { i + 1 } else { SENTINEL })
        });

        Self {
            base,
            free_head: AtomicUsize::new(0),
            free_next,
            allocated: AtomicUsize::new(0),
        }
    }

    /// Acquire a free slot.
    ///
    /// Returns Some((slot_index, slot_ptr)) on success.
    /// Returns None if all SLOT_COUNT slots are currently allocated.
    ///
    /// The returned slot_ptr is valid for SLOT_SIZE bytes and is zero-initialized
    /// on first use (guaranteed by OS + init). On reuse after release(), the slot
    /// may contain stale data — callers must initialize before reading.
    ///
    /// Thread-safe: uses compare_exchange_weak loop on free_head.
    pub fn acquire(&self) -> Option<(usize, *mut u8)> {
        loop {
            let head = self.free_head.load(Ordering::Acquire);
            if head == SENTINEL {
                return None;
            }
            // Load next pointer for head slot
            let next = self.free_next[head].load(Ordering::Relaxed);
            // CAS: try to update free_head from head to next
            match self.free_head.compare_exchange_weak(
                head,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.allocated.fetch_add(1, Ordering::Relaxed);
                    let ptr = unsafe { self.base.add(head * SLOT_SIZE) };
                    return Some((head, ptr));
                }
                Err(_) => continue,
            }
        }
    }

    /// Release a slot back to the free-list.
    ///
    /// After release, the slot may be immediately acquired by another thread.
    /// The caller must ensure no reference to the slot's memory exists after release.
    ///
    /// # Safety
    /// - `index` must be a valid slot index previously returned by acquire().
    /// - No other reference to the slot memory may exist after this call.
    /// - Calling release on a slot that was not acquired is undefined behavior.
    pub unsafe fn release(&self, index: usize) {
        debug_assert!(index < SLOT_COUNT, "release: index {} out of bounds (SLOT_COUNT={})", index, SLOT_COUNT);
        loop {
            let head = self.free_head.load(Ordering::Acquire);
            // Point freed slot's next to current head
            self.free_next[index].store(head, Ordering::Relaxed);
            // CAS: try to set free_head to freed slot
            match self.free_head.compare_exchange_weak(
                head,
                index,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.allocated.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                Err(_) => continue,
            }
        }
    }

    /// Returns the number of currently allocated (in-use) slots.
    #[inline]
    pub fn allocated_count(&self) -> usize {
        self.allocated.load(Ordering::Relaxed)
    }

    /// Returns the number of free slots available.
    #[inline]
    pub fn free_count(&self) -> usize {
        SLOT_COUNT - self.allocated.load(Ordering::Relaxed)
    }

    /// Returns a raw pointer to slot `index`.
    /// Does not acquire or release — pointer arithmetic only.
    ///
    /// # Safety
    /// `index` must be < SLOT_COUNT. The caller must ensure the slot is
    /// currently acquired before dereferencing the returned pointer.
    #[inline]
    pub unsafe fn slot_ptr(&self, index: usize) -> *mut u8 {
        debug_assert!(index < SLOT_COUNT);
        self.base.add(index * SLOT_SIZE)
    }
}

// SAFETY: SlabAllocator is safe to share across threads because all mutable
// state goes through AtomicUsize operations with appropriate ordering.
unsafe impl<const SLOT_SIZE: usize, const SLOT_COUNT: usize> Send
    for SlabAllocator<SLOT_SIZE, SLOT_COUNT> {}
unsafe impl<const SLOT_SIZE: usize, const SLOT_COUNT: usize> Sync
    for SlabAllocator<SLOT_SIZE, SLOT_COUNT> {}

/// Type aliases for the four slab classes defined in Q5.
/// Constants come from src/generated/layout.rs (generated by build.rs).
pub mod slab_classes {
    use super::SlabAllocator;
    use crate::generated::layout::*;

    /// Slab A: GoalContract slots — 256 × 3072 bytes.
    pub type GoalContractSlab = SlabAllocator<GOAL_CONTRACT_BYTES, GOAL_CONTRACT_SLOTS>;

    /// Slab B: GoalState/SubGoalNode slots — 512 × 1024 bytes.
    pub type GoalStateSlab = SlabAllocator<GOAL_STATE_BYTES, GOAL_STATE_SLOTS>;

    /// Slab C: ObservationSnapshot slots — 150 × 320 bytes.
    pub type ObservationSlab = SlabAllocator<OBSERVATION_SLOT_SIZE, OBSERVATION_SLOT_COUNT>;

    /// Slab D: AtomicU64 counter slots — 1024 × 8 bytes.
    pub type AtomicCounterSlab = SlabAllocator<ATOMIC_COUNTER_BYTES, ATOMIC_COUNTER_SLOTS>;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a small slab for tests to avoid large stack allocations
    type TestSlab = SlabAllocator<64, 16>;

    fn make_test_slab() -> TestSlab {
        // Allocate backing memory on the heap (simulating mmap)
        let layout = std::alloc::Layout::from_size_align(64 * 16, 8).unwrap();
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!base.is_null());
        unsafe { TestSlab::init(base) }
    }

    /// Allocate all SLOT_COUNT slots, verify count == SLOT_COUNT,
    /// attempt one more, verify None is returned.
    #[test]
    fn test_exhaustion_returns_none() {
        let slab = make_test_slab();
        let mut slots = Vec::new();
        for _ in 0..16 {
            let slot = slab.acquire();
            assert!(slot.is_some(), "should succeed for first 16");
            slots.push(slot.unwrap());
        }
        assert_eq!(slab.allocated_count(), 16);
        assert_eq!(slab.free_count(), 0);
        // One more should fail
        let overflow = slab.acquire();
        assert!(overflow.is_none(), "slab must return None when exhausted");
    }

    /// Allocate 10 slots, release 5, allocate 5 more.
    /// Verify allocated_count() == 10 and all 5 reallocated pointers
    /// are valid addresses within base .. base + SLOT_SIZE * SLOT_COUNT.
    #[test]
    fn test_release_and_reacquire() {
        let slab = make_test_slab();
        let mut slots = Vec::new();
        for _ in 0..10 {
            slots.push(slab.acquire().unwrap());
        }
        assert_eq!(slab.allocated_count(), 10);

        // Release 5
        for (idx, _ptr) in slots.drain(0..5) {
            unsafe { slab.release(idx) };
        }
        assert_eq!(slab.allocated_count(), 5);

        // Acquire 5 more
        for _ in 0..5 {
            let slot = slab.acquire();
            assert!(slot.is_some());
            let (_idx, ptr) = slot.unwrap();
            let base = slab.base as usize;
            let end = base + 64 * 16;
            assert!(ptr as usize >= base && (ptr as usize) < end, "ptr out of range");
            slots.push((_idx, ptr));
        }
        assert_eq!(slab.allocated_count(), 10);
    }

    /// 8 concurrent threads each acquiring slots simultaneously.
    /// Verify no slot is returned twice.
    #[test]
    fn test_concurrent_acquire_no_duplicates() {
        use std::sync::{Arc, Mutex};
        use std::collections::HashSet;

        // Use a bigger slab for concurrency test
        type BigSlab = SlabAllocator<64, 256>;
        let layout = std::alloc::Layout::from_size_align(64 * 256, 8).unwrap();
        let base = unsafe { std::alloc::alloc_zeroed(layout) };
        let slab = Arc::new(unsafe { BigSlab::init(base) });
        let acquired = Arc::new(Mutex::new(HashSet::new()));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let slab = Arc::clone(&slab);
            let acquired = Arc::clone(&acquired);
            handles.push(std::thread::spawn(move || {
                for _ in 0..32 {
                    if let Some((idx, _ptr)) = slab.acquire() {
                        let mut set = acquired.lock().unwrap();
                        assert!(set.insert(idx), "duplicate slot index: {}", idx);
                    }
                }
            }));
        }
        for h in handles { h.join().unwrap(); }
    }

    /// Verify slot pointer arithmetic: slot_ptr(i) - base == i * SLOT_SIZE.
    #[test]
    fn test_slot_pointer_arithmetic() {
        let slab = make_test_slab();
        let base = slab.base as usize;
        for i in 0..16 {
            let ptr = unsafe { slab.slot_ptr(i) } as usize;
            assert_eq!(ptr - base, i * 64, "slot_ptr({}) arithmetic wrong", i);
        }
    }
}
