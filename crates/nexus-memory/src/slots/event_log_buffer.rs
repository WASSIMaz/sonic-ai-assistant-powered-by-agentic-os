//! event_log_buffer.rs — Write-buffered event log with true ring semantics.
//!
//! PURPOSE (Layer 4, file #22 from plan):
//! EventRecord and EventLogBuffer definitions.
//! Moved here from append_only_region.rs so each slot type has its own file.
//!
//! RING SEMANTICS:
//! write_index is monotonically incrementing (never resets).
//! drain_index is a separate pointer tracking how far the flush thread has read.
//! Physical slot = (claimed_write_index) % capacity.
//! Buffer is considered full only when write_index - drain_index >= capacity.
//! This prevents the bug where the buffer stops accepting events after capacity bytes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use crate::error::BufferError;

/// A single event record stored in the EventLogBuffer.
/// Fixed-size to allow atomic slot claims via fetch_add on byte offset.
#[repr(C)]
pub struct EventRecord {
    /// "ACCESS" or "USE" — discriminates the two event tiers.
    pub event_type: u8,
    /// The slot that was accessed or violated.
    pub slot_id: u64,
    /// The agent that made the access attempt.
    pub agent_id: u64,
    /// Lamport timestamp of the event — enables happened-before ordering.
    pub lamport_ts: u64,
    /// Operation attempted: 0=READ, 1=WRITE, 2=APPEND, 3=COMPUTE.
    pub operation: u8,
    /// For USE events: index into the traceback string table (0 for ACCESS).
    pub traceback_ref: u32,
    /// References the ACCESS event that preceded this USE event, by lamport_ts.
    pub access_event_ref: u64,
    /// 1 if this event is in the emergency buffer awaiting promotion.
    pub promotion_pending: u8,
    /// Padding to fixed size.
    pub _pad: [u8; 21],
}

/// Write-buffered event log backed by a true ring buffer within the APPEND_ONLY mmap.
///
/// Fast path: one fetch_add on write_index + memcpy. No lock. ~10ns.
///
/// TRUE RING SEMANTICS:
/// write_index is monotonically incrementing across the lifetime of the process.
/// drain_index tracks how far the flush thread has consumed.
/// Physical byte offset = (write_index % capacity) — so the buffer wraps correctly.
/// Buffer is full only when (write_index - drain_index) >= capacity.
pub struct EventLogBuffer {
    /// Pointer to the start of the 1MB event log ring within the mmap.
    base: *mut u8,
    /// Total capacity in bytes.
    capacity: usize,
    /// Monotonically incrementing byte counter. Wraps via modulo in physical offset.
    write_index: AtomicUsize,
    /// Drain pointer: bytes consumed by the flush thread so far.
    /// Advances to write_index on each drain() call.
    drain_index: AtomicUsize,
    /// Pointer to the 64KB emergency buffer at the end of the append_only region.
    emergency_base: *mut u8,
    /// Number of bytes used in the emergency buffer.
    emergency_count: AtomicUsize,
    /// Set to true by NexusMemory::init() after SQLite is confirmed ready.
    /// The flush thread checks this before every drain attempt.
    sqlite_ready: Arc<AtomicBool>,
}

impl EventLogBuffer {
    /// Initialize the EventLogBuffer from pointers within the APPEND_ONLY mmap.
    ///
    /// # Panics
    /// Panics if `capacity` is not a multiple of `size_of::<EventRecord>()`.
    /// This is an `assert!` (not `debug_assert!`) because a misaligned capacity
    /// would cause events to straddle the ring boundary in release builds,
    /// leading to silent data corruption. This invariant MUST hold in all builds.
    ///
    /// # Safety
    /// `base` must point to `capacity` bytes and `emergency_base` must point to
    /// `EMERGENCY_BUFFER_BYTES` bytes, both within the APPEND_ONLY mmap region.
    pub unsafe fn from_mmap(
        base: *mut u8,
        capacity: usize,
        emergency_base: *mut u8,
        sqlite_ready: Arc<AtomicBool>,
    ) -> Self {
        assert!(
            capacity % std::mem::size_of::<EventRecord>() == 0,
            "EventLogBuffer capacity must be a multiple of EventRecord size ({} bytes), got {}",
            std::mem::size_of::<EventRecord>(),
            capacity,
        );
        Self {
            base,
            capacity,
            write_index: AtomicUsize::new(0),
            drain_index: AtomicUsize::new(0),
            emergency_base,
            emergency_count: AtomicUsize::new(0),
            sqlite_ready,
        }
    }

    /// Atomically append an EventRecord.
    ///
    /// Uses true ring semantics: write_index monotonically increases, physical offset
    /// = write_index % capacity. Full when (write_index - drain_index) >= capacity.
    ///
    /// Because capacity is guaranteed to be a multiple of EventRecord size (enforced
    /// by the assert! in from_mmap), events NEVER straddle the ring boundary.
    /// The modulo always lands on an EventRecord-aligned offset.
    ///
    /// If sqlite_ready is false: writes to the emergency buffer instead.
    /// If the emergency buffer is also full: returns BufferError::Full.
    ///
    /// Returns the logical byte offset at which the event was written.
    pub fn append(&self, event: &EventRecord) -> Result<usize, BufferError> {
        // Check if we should use emergency buffer
        if !self.sqlite_ready.load(Ordering::Acquire) {
            return self.append_to_emergency(event);
        }

        let event_size = std::mem::size_of::<EventRecord>();

        loop {
            let write = self.write_index.load(Ordering::Acquire);
            let drain = self.drain_index.load(Ordering::Acquire);

            // Full check: ring is full when undrained bytes >= capacity
            let undrained = write.wrapping_sub(drain);
            if undrained + event_size > self.capacity {
                return Err(BufferError::Full);
            }

            // Try to claim the next event_size bytes
            match self.write_index.compare_exchange_weak(
                write,
                write + event_size,
                Ordering::SeqCst,
                Ordering::Acquire,
            ) {
                Ok(claimed) => {
                    // Map logical offset to physical offset via modulo.
                    // Because capacity % event_size == 0 (enforced by assert! in from_mmap),
                    // phys_offset is always EventRecord-aligned and the record never straddles
                    // the ring boundary. No straddle fallback needed.
                    let phys_offset = claimed % self.capacity;

                    // SAFETY: phys_offset + event_size <= capacity (guaranteed by alignment)
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            event as *const EventRecord as *const u8,
                            self.base.add(phys_offset),
                            event_size,
                        );
                    }

                    return Ok(claimed);
                }
                Err(_) => {
                    // Contention — retry
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// Append to emergency buffer (used when SQLite not ready).
    fn append_to_emergency(&self, event: &EventRecord) -> Result<usize, BufferError> {
        let event_size = std::mem::size_of::<EventRecord>();
        let max_emergency = crate::generated::layout::EMERGENCY_BUFFER_BYTES;

        loop {
            let current = self.emergency_count.load(Ordering::Acquire);

            if current + event_size > max_emergency {
                return Err(BufferError::Full);
            }

            match self.emergency_count.compare_exchange_weak(
                current,
                current + event_size,
                Ordering::SeqCst,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    let offset = current;

                    // SAFETY: offset + event_size <= max_emergency
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            event as *const EventRecord as *const u8,
                            self.emergency_base.add(offset),
                            event_size,
                        );
                    }

                    return Ok(current);
                }
                Err(_) => {
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// Drain all pending EventRecords for batch SQLite INSERT.
    ///
    /// Called by the flush thread every 500ms.
    /// Returns all records written since the last drain, in write order.
    /// Advances drain_index to the current write_index.
    pub fn drain(&self) -> Vec<EventRecord> {
        let event_size = std::mem::size_of::<EventRecord>();

        // Atomically read both pointers
        let write = self.write_index.load(Ordering::Acquire);
        let drain = self.drain_index.load(Ordering::Acquire);

        if write == drain {
            return Vec::new();
        }

        let undrained_bytes = write.wrapping_sub(drain);
        let record_count = undrained_bytes / event_size;

        let mut records = Vec::with_capacity(record_count);

        let mut read_logical = drain;
        while read_logical < write {
            let phys = read_logical % self.capacity;

            // SAFETY: phys < capacity, and we only read bytes that were written
            let record = unsafe {
                std::ptr::read(self.base.add(phys) as *const EventRecord)
            };
            records.push(record);
            read_logical += event_size;
        }

        // Advance drain_index to where we stopped reading
        self.drain_index.store(write, Ordering::Release);

        records
    }

    /// Promote all emergency buffer events into the main drain queue.
    ///
    /// Must be called as the FIRST operation after sqlite_ready is set,
    /// before the flush thread's first 500ms cycle fires.
    /// Guarantees no USE event is silently lost during the boot window.
    pub fn promote_emergency(&self) -> Vec<EventRecord> {
        let event_size = std::mem::size_of::<EventRecord>();
        let count = self.emergency_count.load(Ordering::Acquire);

        if count == 0 {
            return Vec::new();
        }

        let record_count = count / event_size;
        let mut records = Vec::with_capacity(record_count);
        let mut offset = 0;

        while offset + event_size <= count {
            // SAFETY: offset + event_size <= count <= EMERGENCY_BUFFER_BYTES
            let record = unsafe {
                std::ptr::read(self.emergency_base.add(offset) as *const EventRecord)
            };
            records.push(record);
            offset += event_size;
        }

        // Reset emergency buffer
        self.emergency_count.store(0, Ordering::Release);

        records
    }

    /// Returns the current write_index (logical byte offset).
    pub fn write_position(&self) -> usize {
        self.write_index.load(Ordering::Acquire)
    }

    /// Returns the current drain_index (logical byte offset).
    pub fn drain_position(&self) -> usize {
        self.drain_index.load(Ordering::Acquire)
    }

    /// Returns the number of undrained bytes in the ring.
    pub fn undrained_bytes(&self) -> usize {
        let write = self.write_index.load(Ordering::Acquire);
        let drain = self.drain_index.load(Ordering::Acquire);
        write.wrapping_sub(drain)
    }
}

// SAFETY: EventLogBuffer accesses shared mmap memory exclusively through
// AtomicUsize operations for write_index and drain_index — no lock needed.
unsafe impl Send for EventLogBuffer {}
unsafe impl Sync for EventLogBuffer {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    /// Helper: allocate aligned memory for testing (simulates mmap).
    unsafe fn alloc_test_buffer(size: usize) -> *mut u8 {
        let layout = Layout::from_size_align(size, 64).unwrap();
        let ptr = alloc_zeroed(layout);
        assert!(!ptr.is_null(), "test buffer allocation failed");
        ptr
    }

    unsafe fn dealloc_test_buffer(ptr: *mut u8, size: usize) {
        let layout = Layout::from_size_align(size, 64).unwrap();
        dealloc(ptr, layout);
    }

    fn make_test_event(event_type: u8, slot_id: u64) -> EventRecord {
        EventRecord {
            event_type,
            slot_id,
            agent_id: 1,
            lamport_ts: 100,
            operation: 0,
            traceback_ref: 0,
            access_event_ref: 0,
            promotion_pending: 0,
            _pad: [0; 21],
        }
    }

    #[test]
    fn test_event_record_size_is_fixed() {
        let size = std::mem::size_of::<EventRecord>();
        // EventRecord must have a stable, known size for ring alignment
        assert!(size > 0, "EventRecord must have nonzero size");
        // Verify it's reasonable (should be around 56-64 bytes)
        assert!(size <= 128, "EventRecord unexpectedly large: {} bytes", size);
    }

    #[test]
    #[should_panic(expected = "EventLogBuffer capacity must be a multiple")]
    fn test_from_mmap_panics_on_misaligned_capacity() {
        let event_size = std::mem::size_of::<EventRecord>();
        // Pick a capacity that is NOT a multiple of event_size
        let bad_capacity = event_size * 10 + 1;

        unsafe {
            let base = alloc_test_buffer(bad_capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            // This MUST panic — the assert! in from_mmap enforces alignment
            let _buf = EventLogBuffer::from_mmap(base, bad_capacity, emergency, sqlite_ready);

            dealloc_test_buffer(base, bad_capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_from_mmap_succeeds_on_aligned_capacity() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 16; // aligned

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            let buf = EventLogBuffer::from_mmap(base, capacity, emergency, sqlite_ready);
            assert_eq!(buf.write_position(), 0);
            assert_eq!(buf.drain_position(), 0);

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_append_and_drain_single_event() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 16;

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            let buf = EventLogBuffer::from_mmap(base, capacity, emergency, sqlite_ready);

            let event = make_test_event(1, 42);
            let offset = buf.append(&event).expect("append must succeed");
            assert_eq!(offset, 0); // first event at logical offset 0

            let drained = buf.drain();
            assert_eq!(drained.len(), 1);
            assert_eq!(drained[0].slot_id, 42);
            assert_eq!(drained[0].event_type, 1);

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_ring_wraps_correctly() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 4; // small ring: only 4 slots

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            let buf = EventLogBuffer::from_mmap(base, capacity, emergency, sqlite_ready);

            // Fill the ring
            for i in 0..4u64 {
                buf.append(&make_test_event(0, i)).expect("append must succeed");
            }

            // Ring is now full — drain everything
            let drained = buf.drain();
            assert_eq!(drained.len(), 4);

            // Now write 4 more — these wrap around
            for i in 10..14u64 {
                buf.append(&make_test_event(0, i)).expect("append after drain must succeed");
            }

            let drained2 = buf.drain();
            assert_eq!(drained2.len(), 4);
            assert_eq!(drained2[0].slot_id, 10);
            assert_eq!(drained2[3].slot_id, 13);

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_full_ring_returns_error() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 2; // only 2 slots

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            let buf = EventLogBuffer::from_mmap(base, capacity, emergency, sqlite_ready);

            // Fill both slots
            buf.append(&make_test_event(0, 1)).unwrap();
            buf.append(&make_test_event(0, 2)).unwrap();

            // Third append must fail
            let result = buf.append(&make_test_event(0, 3));
            assert!(result.is_err(), "append to full ring must return Err");

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_emergency_buffer_used_when_sqlite_not_ready() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 16;

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(false)); // NOT ready

            let buf = EventLogBuffer::from_mmap(base, capacity, emergency, sqlite_ready.clone());

            let event = make_test_event(1, 99);
            let result = buf.append(&event);
            assert!(result.is_ok(), "emergency append must succeed");

            // Now set sqlite_ready and promote
            sqlite_ready.store(true, Ordering::Release);
            let promoted = buf.promote_emergency();
            assert_eq!(promoted.len(), 1);
            assert_eq!(promoted[0].slot_id, 99);

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }

    #[test]
    fn test_concurrent_appends() {
        let event_size = std::mem::size_of::<EventRecord>();
        let capacity = event_size * 1024; // large ring

        unsafe {
            let base = alloc_test_buffer(capacity);
            let emergency = alloc_test_buffer(65536);
            let sqlite_ready = Arc::new(AtomicBool::new(true));

            let buf = Arc::new(EventLogBuffer::from_mmap(
                base, capacity, emergency, sqlite_ready,
            ));

            let mut handles = Vec::new();
            for thread_id in 0..4u64 {
                let buf_clone = Arc::clone(&buf);
                handles.push(std::thread::spawn(move || {
                    for i in 0..50u64 {
                        let event = make_test_event(0, thread_id * 1000 + i);
                        buf_clone.append(&event).expect("concurrent append must succeed");
                    }
                }));
            }

            for h in handles {
                h.join().expect("thread panicked");
            }

            // Should have 200 events total
            let drained = buf.drain();
            assert_eq!(drained.len(), 200, "all 200 concurrent events must be drained");

            dealloc_test_buffer(base, capacity);
            dealloc_test_buffer(emergency, 65536);
        }
    }
}
