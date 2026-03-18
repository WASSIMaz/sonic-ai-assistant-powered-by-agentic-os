//! error.rs — All error types for nexus-memory.
//!
//! This module defines all error types used across the crate:
//! - MmapError: Memory mapping failures (allocation, seal, unseal)
//! - SlabExhausted: No free slots available
//! - LockTimeoutError: Write lock timeout in mutable region
//! - NamespaceViolationError: Unauthorized slot access attempt
//! - BootError: Initialization failures during NexusMemory::init()
//! - BufferError: Event log buffer overflow or corruption

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

// ============================================================================
// MmapError — Memory Mapping Failures
// ============================================================================

/// Errors from memory mapping operations.
///
/// All variants include the region name for debugging and the underlying OS error.
#[derive(Error, Debug)]
pub enum MmapError {
    /// Failed to allocate anonymous shared memory.
    /// On Unix: mmap(MAP_ANONYMOUS) failed.
    /// On Windows: CreateFileMapping + MapViewOfFile failed.
    #[error("mmap allocation failed for region '{name}': {source}")]
    AllocationFailed {
        name: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Failed to seal region as read-only.
    /// On Unix: mprotect(PROT_READ) failed.
    /// On Windows: VirtualProtect(PAGE_READONLY) failed.
    #[error("mmap seal (read-only) failed for region '{name}': {source}")]
    SealFailed {
        name: &'static str,
        #[source]
        source: std::io::Error,
    },

    /// Failed to restore read-write access.
    /// On Unix: mprotect(PROT_READ|PROT_WRITE) failed.
    /// On Windows: VirtualProtect(PAGE_READWRITE) failed.
    #[error("mmap unseal (read-write) failed for region '{name}': {source}")]
    UnsealFailed {
        name: &'static str,
        #[source]
        source: std::io::Error,
    },
}

// ============================================================================
// SlabExhausted — No Free Slots
// ============================================================================

/// All slots in a slab are currently allocated.
///
/// The caller must publish RESOURCE_EXHAUSTED to the Bus and refuse the goal.
/// This is NOT a panic condition — it is expected under high load.
#[derive(Error, Debug)]
#[error("slab '{name}' exhausted: all {slot_count} slots allocated")]
pub struct SlabExhausted {
    pub name: &'static str,
    pub slot_count: usize,
    pub allocated_count: usize,
}

// ============================================================================
// LockTimeoutError — Mutable Region Write Timeout
// ============================================================================

/// Write lock acquisition timed out in the mutable region.
///
/// The holder agent ID identifies who held the lock for too long.
/// Global Lamport timestamp at timeout is included for happened-before analysis.
#[derive(Error, Debug)]
#[error("write lock timeout for slot {slot_id}: held by agent {holder_agent_id} for {waited_ms}ms")]
pub struct LockTimeoutError {
    pub slot_id: u64,
    pub holder_agent_id: u64,
    pub waited_ms: u64,
    pub lamport_ts: u64,
}

// ============================================================================
// NamespaceViolationError — Unauthorized Access
// ============================================================================

/// An agent attempted to access a slot outside its CapabilityToken namespace.
///
/// This error is logged but does NOT crash the process. Instead, a
/// NamespaceViolationSentinel is returned to Python, which detonates on
/// any operation (see violation_sentinel.rs).
#[derive(Error, Debug)]
#[error("namespace violation: agent {agent_id} attempted {operation} on slot {slot_id}")]
pub struct NamespaceViolationError {
    pub agent_id: u64,
    pub slot_id: u64,
    pub operation: &'static str,
    pub lamport_ts: u64,
}

// ============================================================================
// BootError — Initialization Failures
// ============================================================================

/// NexusMemory::init() failed during one of the 9 boot steps.
///
/// Boot is all-or-nothing: if any step fails, the entire initialization
/// aborts and NEXUS is not ready.
#[derive(Error, Debug)]
pub enum BootError {
    /// Step 1 failed: could not map IMMUTABLE region.
    #[error("boot step 1 failed: immutable region mmap failed: {0}")]
    ImmutableMmapFailed(#[source] MmapError),

    /// Step 2 failed: could not map APPEND_ONLY region.
    #[error("boot step 2 failed: append-only region mmap failed: {0}")]
    AppendOnlyMmapFailed(#[source] MmapError),

    /// Step 3 failed: could not map MUTABLE region.
    #[error("boot step 3 failed: mutable region mmap failed: {0}")]
    MutableMmapFailed(#[source] MmapError),

    /// Step 4 failed: could not map COMPUTE region.
    #[error("boot step 4 failed: compute region mmap failed: {0}")]
    ComputeMmapFailed(#[source] MmapError),

    /// Step 5 failed: SQLite initialization failed or timed out.
    #[error("boot step 5 failed: SQLite initialization failed after {timeout_ms}ms: {source}")]
    SqliteInitFailed {
        timeout_ms: u64,
        #[source]
        source: rusqlite::Error,
    },

    /// Step 6 failed: SQLite ready flag could not be set.
    #[error("boot step 6 failed: SQLite ready flag not set")]
    SqliteReadyFlagNotSet,

    /// Step 7 failed: emergency buffer promotion failed.
    #[error("boot step 7 failed: emergency buffer promotion failed: {0}")]
    EmergencyBufferPromotionFailed(String),

    /// Step 8 failed: flush thread could not be started.
    #[error("boot step 8 failed: flush thread spawn failed")]
    FlushThreadSpawnFailed,

    /// Step 9 failed: SCHEMA_HASH mismatch between mmap header and layout.rs.
    #[error("boot step 9 failed: SCHEMA_HASH mismatch: expected {expected}, found {found}")]
    SchemaHashMismatch {
        expected: String,
        found: String,
    },
}

// ============================================================================
// BufferError — Event Log Failures
// ============================================================================

/// Errors from the event log buffer in the APPEND_ONLY region.
#[derive(Error, Debug)]
pub enum BufferError {
    /// Ring buffer was lapped by writer during read.
    /// Reader tried to read but write_index advanced by more than the ring capacity.
    #[error("ring buffer lapped: write_index advanced {delta} slots during read")]
    RingLapped { delta: u64 },

    /// Emergency buffer is full and SQLite is not ready.
    /// This is a critical failure — events may be lost.
    #[error("emergency buffer full: {used}/{capacity} bytes, SQLite not ready")]
    EmergencyBufferFull { used: usize, capacity: usize },

    /// Event record size exceeds buffer capacity.
    #[error("event record too large: {size} bytes exceeds capacity {capacity}")]
    EventTooLarge { size: usize, capacity: usize },
}

// ============================================================================
// Global Lamport Clock
// ============================================================================

/// Global Lamport clock for happened-before ordering across all events.
///
/// Used in:
/// - LockTimeoutError::lamport_ts
/// - NamespaceViolationError::lamport_ts
/// - EventRecord::lamport_ts
/// - MutableSlot::last_modified_lamport
///
/// The clock is monotonically increasing and advanced on every successful
/// write to the mutable region.
static GLOBAL_LAMPORT: AtomicU64 = AtomicU64::new(0);

/// Advance the global Lamport clock and return the new value.
/// Called on every successful write to mutable region.
#[inline]
pub fn advance_lamport() -> u64 {
    GLOBAL_LAMPORT.fetch_add(1, Ordering::SeqCst) + 1
}

/// Get the current Lamport timestamp without advancing.
#[inline]
pub fn current_lamport() -> u64 {
    GLOBAL_LAMPORT.load(Ordering::SeqCst)
}

/// Set the Lamport clock to at least the given value.
/// Used when receiving events from other processes to maintain causality.
#[inline]
pub fn sync_lamport(other: u64) {
    GLOBAL_LAMPORT.fetch_max(other, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lamport_advances_monotonically() {
        let t1 = advance_lamport();
        let t2 = advance_lamport();
        let t3 = advance_lamport();
        assert!(t2 > t1, "Lamport must advance: {} should be > {}", t2, t1);
        assert!(t3 > t2, "Lamport must advance: {} should be > {}", t3, t2);
    }

    #[test]
    fn test_lamport_sync_increases() {
        let before = current_lamport();
        sync_lamport(before + 1000);
        let after = current_lamport();
        assert!(after >= before + 1000, "sync_lamport must increase clock");
    }

    #[test]
    fn test_lamport_sync_no_decrease() {
        let before = current_lamport();
        sync_lamport(0); // Should not decrease
        let after = current_lamport();
        assert!(after >= before, "sync_lamport must not decrease clock");
    }
}