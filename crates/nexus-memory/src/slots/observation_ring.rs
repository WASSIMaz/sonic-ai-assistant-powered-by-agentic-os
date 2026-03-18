//! observation_ring.rs — 150-slot lock-free observation ring buffer.
//!
//! DESIGN:
//! The ring is backed by 150 contiguous ObservationSnapshot slots within
//! the APPEND_ONLY mmap region. A single AtomicUsize write_index increments
//! monotonically on every write. The physical slot index is write_index % 150.
//! When the ring wraps (write_index reaches 150 for the first time, then 300,
//! etc.), the oldest slot is silently overwritten. This is by design — the ring
//! is sensory memory covering exactly 30 seconds of observation history.
//!
//! WRITE PROTOCOL:
//! 1. fetch_add(1, SeqCst) on write_index — atomically claims a slot index.
//! 2. Compute physical = (claimed_index - 1) % 150.
//! 3. Write ObservationSnapshot into slots[physical] via ptr::copy_nonoverlapping.
//! No lock. No CAS loop. One atomic increment per write.
//!
//! SEQLOCK READ PROTOCOL (from Q14):
//! To detect if the ring lapped the reader during a read:
//! 1. Before reading: snapshot pre_index = write_index.load(SeqCst).
//! 2. Read the slot.
//! 3. After reading: check post_index = write_index.load(SeqCst).
//! 4. If post_index - pre_index >= 150, the ring lapped the reader.
//!    The read must be retried (up to MAX_RETRIES) or return RingLapped signal.
//! This prevents returning partial data from a slot overwritten mid-read.
//!
//! MEMORY LAYOUT:
//! 150 ObservationSnapshot structs at consecutive 320-byte offsets within mmap.
//! The 64-byte _cache_pad at the end of each struct ensures no false sharing
//! between adjacent slots when multiple CPU Worker Pool threads read the ring.
//!
//! RING FOOTPRINT:
//! 150 × 320 bytes = 48,000 bytes (~47KB) — fits entirely in L3 cache.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of SeqLock retries before giving up and returning RingLapped.
const MAX_SEQLOCK_RETRIES: usize = 3;

/// The observation snapshot struct — one 200ms Oracle poll captured here.
///
/// # Safety invariants
/// - #[repr(C)] guarantees field layout matches nexus_layout.toml and _layout.py.
/// - Total size must be exactly 320 bytes — enforced by the compile-time assert below.
/// - The 64-byte _cache_pad ensures no false sharing with the adjacent slot.
/// - All fields are plain data (no pointers, no references) — safe to memcpy.
#[repr(C)]
pub struct ObservationSnapshot {
    /// Monotonic wall clock at time of Oracle poll (nanoseconds since process start).
    pub timestamp_ns: u64,                // 8 bytes

    /// UTF-8 window title, null-padded to 64 bytes. Sourced from getActiveWindow().
    pub active_window: [u8; 64],          // 64 bytes

    /// Process name of the active window's process, null-padded. From OS process table.
    pub active_process: [u8; 32],         // 32 bytes

    /// PID of the active window's process.
    pub active_pid: u32,                  // 4 bytes

    /// System-wide CPU utilization as a percentage (0.0–100.0). From /proc/stat on Linux.
    pub total_cpu_pct: f32,               // 4 bytes

    /// Physical memory currently in use, in megabytes. From /proc/meminfo on Linux.
    pub memory_used_mb: u32,              // 4 bytes

    /// Total physical memory, in megabytes.
    pub memory_total_mb: u32,             // 4 bytes

    /// Free disk space on the primary volume, in gigabytes. From statvfs.
    pub disk_free_gb: f32,                // 4 bytes

    /// Disk pressure level: 0=LOW, 1=MED, 2=HIGH.
    pub disk_pressure: u8,                // 1 byte

    /// Network reachability: 0=unreachable, 1=reachable.
    pub network_reachable: u8,            // 1 byte

    /// Network round-trip latency in milliseconds. 0 if unreachable.
    pub network_latency_ms: u16,          // 2 bytes

    /// Clipboard content type: 0=EMPTY, 1=TEXT, 2=IMAGE.
    pub clipboard_type: u8,               // 1 byte

    /// Alignment padding to 4-byte boundary.
    pub _pad: [u8; 3],                    // 3 bytes

    /// Up to 4 goal IDs currently active at the time of this snapshot.
    pub goal_ids_active: [u32; 4],        // 16 bytes

    /// The goal that causally triggered this observation (for Chronicler attribution).
    pub causal_goal_id: u32,              // 4 bytes

    /// The agent that triggered this observation (Chronicler attribution).
    pub causal_agent_id: u32,             // 4 bytes

    /// Number of installed applications at the time of this snapshot.
    /// Written atomically by MachineProfile refresh. Required by Contextualizer
    /// to correctly reshape the capability matrix as (live_app_count, 512).
    pub live_app_count: u32,              // 4 bytes

    /// Padding to reach exactly 256 bytes of content.
    pub _struct_pad: [u8; 32],            // 32 bytes

    /// Cache-line separation padding — prevents false sharing with adjacent slots.
    /// With this pad, each slot occupies exactly 320 bytes = 5 × 64-byte cache lines.
    pub _cache_pad: [u8; 64],             // 64 bytes
}
// Total: 8+64+32+4+4+4+4+4+1+1+2+1+3+16+4+4+4+32+64 = 256 content + 64 pad = 320 bytes.
const _OBSERVATION_SNAPSHOT_SIZE_CHECK: () = assert!(
    std::mem::size_of::<ObservationSnapshot>() == 320,
    "ObservationSnapshot must be exactly 320 bytes — update nexus_layout.toml if changed"
);

/// Result of a ring read operation.
pub enum ReadResult {
    /// The slot was read successfully.
    Ok(ObservationSnapshot),

    /// The ring lapped the reader during the read — all retries exhausted.
    /// The returned snapshot may be partially from the new write.
    /// The caller should treat this as a cache miss and use the latest slot instead.
    RingLapped,
}

/// The 150-slot observation ring backed by the APPEND_ONLY mmap region.
pub struct ObservationRing {
    /// Pointer to the first slot (within APPEND_ONLY mmap, at OBSERVATION_RING_OFFSET).
    slots: *mut ObservationSnapshot,

    /// Monotonically incrementing write counter. Physical index = write_index % 150.
    /// Incremented with SeqCst ordering on every write.
    write_index: AtomicUsize,
}

impl ObservationRing {
    /// Initialize the ring from a raw pointer into the APPEND_ONLY mmap region.
    ///
    /// # Safety
    /// `base` must point to at least OBSERVATION_SLOT_COUNT * OBSERVATION_SLOT_SIZE bytes
    /// of writable mmap memory, starting at OBSERVATION_RING_OFFSET within the region.
    pub unsafe fn from_mmap(base: *mut u8) -> Self {
        todo!()
    }

    /// Write a new ObservationSnapshot into the ring.
    ///
    /// Protocol: fetch_add(1, SeqCst) claims the next write index.
    /// Physical slot = (claimed - 1) % SLOT_COUNT.
    /// Silent overwrite on ring wrap — this is correct behavior (sensory memory).
    /// One atomic increment. No lock. No CAS retry.
    pub fn write(&self, snapshot: ObservationSnapshot) {
        todo!()
    }

    /// Read the snapshot at a specific physical slot index (0..150).
    ///
    /// Uses SeqLock protocol to detect ring lap:
    /// - Snapshot write_index before reading.
    /// - Read the slot via memcpy.
    /// - Check if write_index advanced by >= SLOT_COUNT during the read.
    /// - If lapped: retry up to MAX_SEQLOCK_RETRIES times before returning RingLapped.
    pub fn read(&self, slot_index: usize) -> ReadResult {
        todo!()
    }

    /// Returns the current monotonic write_index (NOT modulo SLOT_COUNT).
    /// Used by readers to implement the SeqLock check.
    #[inline]
    pub fn current_write_index(&self) -> usize {
        todo!()
    }

    /// Returns the `count` most recent snapshots in chronological order (oldest first).
    ///
    /// Internally uses the SeqLock protocol for each slot read.
    /// Skips slots where RingLapped is returned and uses the retry mechanism.
    /// If `count` > SLOT_COUNT (150), clamps to SLOT_COUNT.
    pub fn recent(&self, count: usize) -> Vec<ObservationSnapshot> {
        todo!()
    }
}

// SAFETY: ObservationRing wraps a raw pointer to mmap memory. The ring is safe
// to share across threads because the write protocol uses a single SeqCst atomic
// increment and the read protocol uses the SeqLock check to detect concurrent writes.
unsafe impl Send for ObservationRing {}
unsafe impl Sync for ObservationRing {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write 151 snapshots with sequential timestamp_ns. Call recent(150).
    /// Verify first element has timestamp_ns == 2 (slot 0 overwritten by write 151).
    /// Verify last element has timestamp_ns == 151.
    #[test]
    fn test_ring_wrap_overwrites_oldest() {
        todo!()
    }

    /// One writer at full speed, one reader calling recent(10) every 1ms for 5s.
    /// Verify was_lapped is detected and retried correctly.
    /// Verify no returned snapshot has timestamps that violate monotonic order.
    #[test]
    fn test_seqlock_detects_lapped_reader() {
        todo!()
    }

    /// Write 150 snapshots. Snapshot write_index. Sleep for 150+ more writes.
    /// Attempt to read original slot. Verify ReadResult::RingLapped is returned.
    #[test]
    fn test_full_lap_returns_ring_lapped() {
        todo!()
    }
}