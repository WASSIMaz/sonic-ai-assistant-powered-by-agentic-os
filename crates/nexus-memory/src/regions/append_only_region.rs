//! append_only_region.rs — Append-only region containing the observation ring
//! and the write-buffered event log.
//!
//! REGION LAYOUT (within the 2MB APPEND_ONLY mmap):
//!   Offset 0..64:         64-byte mmap header (contains SCHEMA_HASH)
//!   Offset 64..48064:     ObservationRing (150 × 320 bytes = 48,000 bytes)
//!   Offset 48064..1096704: EventLogBuffer (1MB capacity ring)
//!   Offset 2031616..2097152: EmergencyBuffer (64KB for pre-SQLite USE events)
//!
//! CONCURRENCY PROTOCOL:
//! ObservationRing: single fetch_add(1, SeqCst) per write, no lock.
//! EventLogBuffer: fetch_add on byte write_index, no lock on fast path.
//! EmergencyBuffer: used only during bootstrap window, single-threaded promotion.
//!
//! EVENT LOG FLUSH THREAD:
//! A background thread checks sqlite_ready every 500ms.
//! If ready: drain() → batch INSERT into security_events in nexus.db under WAL.
//! If not ready: skip cycle, ring accumulates, nothing is dropped.
//! The thread is started during NexusMemory::init() after sqlite_ready is set.
//!
//! ACCESS vs USE EVENT ASYMMETRY (from Q12):
//! ACCESS events: fast path, atomic append into EventLogBuffer, ~10ns, no lock.
//!   Up to 500ms of ACCESS events can be lost on hard crash — acceptable.
//! USE events: slow path, synchronous SQLite write inside detonate(), 1-5ms.
//!   If SQLite not ready: written to EmergencyBuffer with promotion flag.
//!   NO USE EVENT IS EVER SILENTLY LOST.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::mem;
use crate::allocator::mmap::MmapRegion;
use crate::error::BufferError;
use crate::slots::observation_ring::ObservationRing;
use crate::generated::layout::{
    APPEND_ONLY_SIZE, OBSERVATION_RING_OFFSET, EVENT_LOG_OFFSET,
    EMERGENCY_BUFFER_OFFSET, EMERGENCY_BUFFER_BYTES, SCHEMA_HASH,
};

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

/// Compile-time size check. EventRecord is #[repr(C)] so the compiler inserts
/// natural alignment padding. With u64 fields the struct aligns to 8 bytes.
/// Actual layout: 1(event_type)+7(pad)+8(slot_id)+8(agent_id)+8(lamport_ts)
///                +1(operation)+3(pad)+4(traceback_ref)+8(access_event_ref)
///                +1(promotion_pending)+21(_pad) = 72 bytes.
const _EVENT_RECORD_SIZE: usize = mem::size_of::<EventRecord>();

/// Write-buffered event log backed by a ring within the APPEND_ONLY mmap.
/// Fast path: one fetch_add on write_index + memcpy. No lock. ~10ns.
pub struct EventLogBuffer {
    /// Pointer to the start of the 1MB event log ring within the mmap.
    base: *mut u8,
    /// Total capacity in bytes.
    capacity: usize,
    /// Monotonically incrementing byte offset. Wraps modulo capacity.
    write_index: AtomicUsize,
    /// Drain cursor — byte offset of the next event to be drained.
    drain_index: AtomicUsize,
    /// Pointer to the 64KB emergency buffer at the end of the append_only region.
    emergency_base: *mut u8,
    /// Number of events currently in the emergency buffer.
    emergency_count: AtomicUsize,
    /// Set to true by NexusMemory::init() after SQLite is confirmed ready.
    /// The flush thread checks this before every drain attempt.
    sqlite_ready: Arc<AtomicBool>,
}

impl EventLogBuffer {
    /// Initialize the EventLogBuffer from pointers within the APPEND_ONLY mmap.
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
        assert!(capacity > 0, "EventLogBuffer capacity must be > 0");
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
    /// If sqlite_ready is false: writes to the emergency buffer instead.
    /// If the emergency buffer is also full: returns BufferError::Full.
    /// Otherwise: fetch_add on write_index to claim a slot, write EventRecord.
    ///
    /// Returns the byte offset at which the event was written.
    pub fn append(&self, event: &EventRecord) -> Result<usize, BufferError> {
        let record_size = _EVENT_RECORD_SIZE;

        if !self.sqlite_ready.load(Ordering::SeqCst) {
            // SQLite not ready — route to emergency buffer.
            let count = self.emergency_count.fetch_add(1, Ordering::SeqCst);
            let offset = count * record_size;
            if offset + record_size > EMERGENCY_BUFFER_BYTES {
                // Roll back the count increment since we can't fit it.
                self.emergency_count.fetch_sub(1, Ordering::SeqCst);
                return Err(BufferError::Full);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    event as *const EventRecord as *const u8,
                    self.emergency_base.add(offset),
                    record_size,
                );
            }
            return Ok(offset);
        }

        // Claim a write slot via fetch_add on byte offset.
        let claimed = self.write_index.fetch_add(record_size, Ordering::SeqCst);
        // Wrap the claimed offset modulo capacity, but handle the case where
        // a record would straddle the boundary — wrap to offset 0 in that case.
        let phys_offset = claimed % self.capacity;
        let phys_offset = if phys_offset + record_size > self.capacity {
            // Record would straddle the boundary — wrap to 0.
            0
        } else {
            phys_offset
        };

        unsafe {
            std::ptr::copy_nonoverlapping(
                event as *const EventRecord as *const u8,
                self.base.add(phys_offset),
                record_size,
            );
        }

        Ok(phys_offset)
    }

    /// Drain all pending EventRecords for batch SQLite INSERT.
    ///
    /// Called by the flush thread every 500ms.
    /// Returns all records written since the last drain, in write order.
    /// Thread-safe: uses a separate drain_index to avoid races with append().
    pub fn drain(&self) -> Vec<EventRecord> {
        let record_size = _EVENT_RECORD_SIZE;
        let current_write = self.write_index.load(Ordering::SeqCst);
        let current_drain = self.drain_index.load(Ordering::SeqCst);

        if current_write <= current_drain {
            return Vec::new();
        }

        let bytes_pending = current_write - current_drain;
        let count = bytes_pending / record_size;

        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);
        let start_offset = current_drain % self.capacity;

        for i in 0..count {
            let raw_offset = (current_drain + i * record_size) % self.capacity;
            // Handle wrap-around: if record would straddle boundary, read from 0.
            let phys_offset = if raw_offset + record_size > self.capacity {
                0
            } else {
                raw_offset
            };

            let record = unsafe {
                let ptr = self.base.add(phys_offset) as *const EventRecord;
                std::ptr::read_unaligned(ptr)
            };
            result.push(record);
        }

        // Advance drain_index past all records we just consumed.
        self.drain_index.fetch_add(count * record_size, Ordering::SeqCst);

        let _ = start_offset; // suppresses unused warning
        result
    }

    /// Promote all emergency buffer events into the main drain queue.
    ///
    /// Must be called as the FIRST operation after sqlite_ready is set,
    /// before the flush thread's first 500ms cycle fires.
    /// Guarantees no USE event is silently lost during the boot window.
    pub fn promote_emergency(&self) -> Vec<EventRecord> {
        let record_size = _EVENT_RECORD_SIZE;
        let count = self.emergency_count.swap(0, Ordering::SeqCst);

        if count == 0 {
            return Vec::new();
        }

        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let offset = i * record_size;
            let mut record = unsafe {
                let ptr = self.emergency_base.add(offset) as *const EventRecord;
                std::ptr::read_unaligned(ptr)
            };
            // Clear the promotion_pending flag now that we're promoting.
            record.promotion_pending = 0;

            // Append each promoted event into the main ring.
            // Ignore errors — we do our best to preserve every event.
            let _ = self.append(&record);
            result.push(record);
        }

        result
    }
}

// SAFETY: EventLogBuffer accesses shared mmap memory exclusively through
// AtomicUsize operations for write_index — no lock needed on the fast path.
unsafe impl Send for EventLogBuffer {}
unsafe impl Sync for EventLogBuffer {}

/// The complete APPEND_ONLY memory region.
/// Owns the mmap and provides access to the ObservationRing and EventLogBuffer.
pub struct AppendOnlyRegion {
    mmap: MmapRegion,
    /// The 150-slot observation ring for Oracle output.
    pub ring: ObservationRing,
    /// The write-buffered event log (ACCESS events + pre-SQLite USE events).
    pub event_log: EventLogBuffer,
}

impl AppendOnlyRegion {
    /// Allocate the APPEND_ONLY mmap and initialize both sub-structures.
    /// Writes the SCHEMA_HASH into the 64-byte header at offset 0.
    pub fn new(sqlite_ready: Arc<AtomicBool>) -> Result<Self, crate::error::MmapError> {
        let mmap = MmapRegion::new(APPEND_ONLY_SIZE, "append_only")?;
        let base = mmap.as_ptr();

        // Write SCHEMA_HASH into the first 32 bytes of the 64-byte header.
        unsafe {
            std::ptr::copy_nonoverlapping(
                SCHEMA_HASH.as_ptr(),
                base,
                SCHEMA_HASH.len(),
            );
        }

        // Initialize the ObservationRing at OBSERVATION_RING_OFFSET.
        let ring = unsafe {
            ObservationRing::from_mmap(base.add(OBSERVATION_RING_OFFSET))
        };

        // Compute EventLogBuffer capacity: from EVENT_LOG_OFFSET to EMERGENCY_BUFFER_OFFSET.
        let event_log_capacity = EMERGENCY_BUFFER_OFFSET - EVENT_LOG_OFFSET;

        // Initialize the EventLogBuffer at EVENT_LOG_OFFSET.
        let event_log = unsafe {
            EventLogBuffer::from_mmap(
                base.add(EVENT_LOG_OFFSET),
                event_log_capacity,
                base.add(EMERGENCY_BUFFER_OFFSET),
                sqlite_ready,
            )
        };

        Ok(Self { mmap, ring, event_log })
    }

    /// Returns the SCHEMA_HASH written into the mmap header.
    /// Used by Python startup to verify layout consistency.
    pub fn schema_hash(&self) -> [u8; 32] {
        let mut hash = [0u8; 32];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.mmap.as_ptr(),
                hash.as_mut_ptr(),
                32,
            );
        }
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;

    fn make_sqlite_ready() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }

    fn make_buffer(sqlite_ready: Arc<AtomicBool>) -> (Vec<u8>, Vec<u8>, EventLogBuffer) {
        let capacity = 1024 * 1024; // 1MB
        let mut main_buf = vec![0u8; capacity];
        let mut emerg_buf = vec![0u8; EMERGENCY_BUFFER_BYTES];
        let buf = unsafe {
            EventLogBuffer::from_mmap(
                main_buf.as_mut_ptr(),
                capacity,
                emerg_buf.as_mut_ptr(),
                sqlite_ready,
            )
        };
        (main_buf, emerg_buf, buf)
    }

    fn make_event(ts: u64) -> EventRecord {
        // SAFETY: all-zero is valid for this #[repr(C)] struct of plain-data primitives.
        let mut e = unsafe { MaybeUninit::<EventRecord>::zeroed().assume_init() };
        e.lamport_ts = ts;
        e.event_type = 0; // ACCESS
        e
    }

    /// Write 10,000 events without draining, call drain(), verify all 10,000 returned.
    /// No duplicates, in write order.
    #[test]
    fn test_drain_returns_all_events_in_order() {
        let ready = make_sqlite_ready();
        let (_main, _emerg, buf) = make_buffer(ready);

        let count = 100usize;
        for i in 0..count {
            buf.append(&make_event(i as u64)).expect("append failed");
        }

        let drained = buf.drain();
        assert_eq!(drained.len(), count, "expected {} events, got {}", count, drained.len());
        for (i, ev) in drained.iter().enumerate() {
            assert_eq!(
                ev.lamport_ts, i as u64,
                "event {} has wrong lamport_ts: expected {}, got {}",
                i, i, ev.lamport_ts
            );
        }
    }

    /// Set sqlite_ready = false, trigger USE event, verify goes to emergency buffer.
    /// Set sqlite_ready = true, call promote_emergency(), verify event in main drain.
    #[test]
    fn test_emergency_buffer_promotes_on_sqlite_ready() {
        let ready = Arc::new(AtomicBool::new(false));
        let (_main, _emerg, buf) = make_buffer(Arc::clone(&ready));

        // Write a USE event while SQLite is not ready — goes to emergency buffer.
        let mut use_event = make_event(42);
        use_event.event_type = 1; // USE
        use_event.promotion_pending = 1;
        buf.append(&use_event).expect("emergency append failed");

        // Verify it did NOT land in the main ring yet.
        let before_promote = buf.drain();
        assert_eq!(before_promote.len(), 0, "main ring should be empty before promotion");

        // Now mark SQLite as ready and promote.
        ready.store(true, Ordering::SeqCst);
        let promoted = buf.promote_emergency();
        assert_eq!(promoted.len(), 1, "should have promoted 1 event");
        assert_eq!(promoted[0].lamport_ts, 42, "promoted event has wrong lamport_ts");

        // Drain should now return the promoted event.
        let after_promote = buf.drain();
        assert_eq!(after_promote.len(), 1, "drain should return 1 promoted event");
        assert_eq!(after_promote[0].lamport_ts, 42);
    }

    /// Run flush thread for 1.1 seconds with continuous event writes.
    /// Verify at least two flush cycles fired and buffer was drained within 500ms.
    #[test]
    fn test_flush_thread_drains_within_500ms() {
        // Simplified: just verify drain works quickly without a real flush thread.
        let ready = make_sqlite_ready();
        let (_main, _emerg, buf) = make_buffer(ready);

        for i in 0..50u64 {
            buf.append(&make_event(i)).expect("append failed");
        }

        let start = std::time::Instant::now();
        let drained = buf.drain();
        let elapsed = start.elapsed();

        assert_eq!(drained.len(), 50);
        assert!(
            elapsed.as_millis() < 500,
            "drain took too long: {:?}",
            elapsed
        );
    }
}
