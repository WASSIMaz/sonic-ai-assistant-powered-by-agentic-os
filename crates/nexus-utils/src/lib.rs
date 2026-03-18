//! # NEXUS Utilities
//!
//! Core utility primitives for the NEXUS Agentic Operating System.
//!
//! This crate provides low-level concurrency primitives, timing utilities,
//! and data structures optimized for the NEXUS kernel and agent space.
//!
//! ## Architecture
//!
//! The NEXUS Agentic OS is built around several key abstractions:
//!
//! 1. **Cognitive Kernel** (Rust): Low-level coordination, scheduling, memory
//!    management, and system observation. Uses utilities from this crate.
//!
//! 2. **Agent Space** (Python): High-level planning, LLM integration, and
//!    agent orchestration. Interfaces with the kernel via typed buffers.
//!
//! 3. **Security Kernel** (Python): Action gating, semantic checking, and
//!    audit logging. Validates actions before execution.
//!
//! This crate provides the foundational utilities used throughout the
//! Rust-based cognitive kernel.
//!
//! ## Modules
//!
//! ### `error` - Error Handling
//!
//! Comprehensive error types with rich context, backtrace support, and
//! error chaining. Used throughout NEXUS for consistent error handling.
//!
//! ```rust
//! use nexus_utils::error::{NexusError, NexusResult, ErrorKind};
//! use nexus_utils::ensure;
//!
//! fn divide(a: f64, b: f64) -> NexusResult<f64> {
//!     ensure!(b != 0.0, ErrorKind::InvalidInput, "division by zero");
//!     Ok(a / b)
//! }
//! ```
//!
//! ### `atomic` - Atomic Primitives
//!
//! Enhanced atomic types optimized for NEXUS workloads:
//!
//! - `AtomicTimestamp` for monotonic timestamp tracking (Lamport clocks)
//! - `AtomicCounter` for high-performance counting with saturation
//! - `AtomicCell` for lock-free single-value containers
//! - `PaddedAtomicU64` to prevent false sharing
//!
//! ```rust
//! use nexus_utils::atomic::{AtomicTimestamp, AtomicCounter};
//! use std::sync::atomic::Ordering;
//!
//! let ts = AtomicTimestamp::new(0);
//! ts.advance(1000); // Monotonic - never goes backwards
//!
//! let counter = AtomicCounter::new(0);
//! counter.increment(Ordering::Relaxed);
//! ```
//!
//! ### `timing` - Timing Utilities
//!
//! High-resolution timing primitives for:
//!
//! - Heartbeat monitoring (200ms Oracle poll cycle)
//! - Drift-corrected periodic timers
//! - Deadline tracking for goal execution
//! - Adaptive intervals based on execution phase
//!
//! ```rust
//! use nexus_utils::timing::{HeartbeatTimer, Deadline, DriftCorrectedTimer};
//! use std::time::Duration;
//!
//! // Heartbeat for kernel health monitoring
//! let heartbeat = HeartbeatTimer::new(Duration::from_secs(1));
//! heartbeat.beat();
//!
//! if heartbeat.is_stalled(Duration::from_secs(3)) {
//!     // Kernel is stalled - trigger recovery
//! }
//!
//! // Deadline for goal execution
//! let deadline = Deadline::from_now(Duration::from_secs(30));
//! let progress = deadline.progress_ratio();
//! ```
//!
//! ### `rwlock` - Read-Write Locks
//!
//! Custom RwLock implementations optimized for NEXUS:
//!
//! - Writer-priority locks (prevents writer starvation)
//! - Fair locks with ticket-based scheduling
//! - Upgradable read locks
//! - Instrumented locks with contention metrics
//!
//! ```rust
//! use nexus_utils::rwlock::RwLock;
//!
//! let lock = RwLock::new(vec![1, 2, 3]);
//!
//! // Multiple readers can hold the lock
//! let r1 = lock.read();
//! let r2 = lock.read();
//!
//! // Writers get priority over new readers
//! drop(r1);
//! drop(r2);
//! let mut w = lock.write();
//! w.push(4);
//! ```
//!
//! ### `ring_buffer` - Lock-Free Ring Buffers
//!
//! High-performance MPMC ring buffers for the event bus:
//!
//! - Lock-free implementation using atomics
//! - Cache-line aligned slots to prevent false sharing
//! - Bounded and unbounded variants
//! - Typed buffers for event discrimination
//!
//! ```rust
//! use nexus_utils::ring_buffer::MpmcRingBuffer;
//!
//! let buffer = MpmcRingBuffer::new(1024);
//!
//! // Multiple producers can push
//! buffer.push(42).unwrap();
//!
//! // Multiple consumers can pop
//! assert_eq!(buffer.pop(), Some(42));
//! ```
//!
//! ### `dash_map` - Concurrent Hash Maps
//!
//! Sharded concurrent hash maps for high-throughput workloads:
//!
//! - Fixed sharding to reduce lock contention
//! - Lock-free reads with RwLock per shard
//! - Multi-value maps for topic subscriptions
//! - BiMaps for bidirectional lookups
//!
//! ```rust
//! use nexus_utils::dash_map::DashMap;
//!
//! let map = DashMap::new();
//!
//! // Concurrent access is safe
//! map.insert("key", 42);
//! assert!(map.get(&"key").is_some());
//! ```
//!
//! ## Feature Flags
//!
//! - `tracing` (default): Enables tracing support for debugging
//! - `serde` (default): Enables serialization/deserialization support
//! - `nightly`: Enables nightly-only optimizations
//!
//! ## Design Principles
//!
//! 1. **Zero-allocation hot paths**: Core operations never allocate
//! 2. **Cache-friendly**: Structures aligned to cache lines
//! 3. **Writer priority**: RwLocks prevent writer starvation
//! 4. **Monotonic timestamps**: Clock values never go backwards
//! 5. **Lock-free where possible**: Use atomics instead of mutexes

#![warn(rust_2018_idioms)]
#![deny(unsafe_op_in_unsafe_fn)]
#![allow(clippy::module_inception)]
#![allow(unsafe_code)] // Required for atomic pointer operations and MaybeUninit writes

// ============================================================================
// SAFETY: Unsafe Code Justification
// ============================================================================
//
// This crate uses unsafe code in the following modules:
//
// 1. atomic.rs - Atomic pointer operations:
//    - AtomicRef uses raw pointers for lock-free atomic updates
//    - AtomicCell uses MaybeUninit for safe initialization
//    - All atomic operations maintain memory ordering invariants
//
// 2. ring_buffer.rs - Lock-free ring buffer:
//    - Cache-padded slots use MaybeUninit for zero-copy reads
//    - Atomic indices ensure safe concurrent access
//    - Readers only access initialized slots (guaranteed by head/tail indices)
//
// 3. rwlock.rs - Custom RwLock implementations:
//    - FairRwLock uses UnsafeCell for interior mutability
//    - Safety guaranteed by RawRwLock trait implementation
//    - All access is gated through proper lock acquisition
//
// Invariants:
// - All atomic operations use appropriate Ordering (Acquire/Release/SeqCst)
// - MaybeUninit writes are only read after proper initialization
// - Pointer provenance is maintained throughout
// - No data races are possible due to atomic synchronization

// ============================================================================
// Modules
// ============================================================================

pub mod atomic;
pub mod dash_map;
pub mod error;
pub mod ring_buffer;
pub mod rwlock;
pub mod timing;

// ============================================================================
// Re-exports
// ============================================================================

// Re-export commonly used types at the crate root for convenience
pub use atomic::{
    AtomicCell, AtomicCounter, AtomicInt, AtomicOption, AtomicPointer, AtomicRef, AtomicTimestamp,
    PaddedAtomicU64, PaddedAtomicUsize, TypedAtomic,
};

pub use error::{
    AccessOp, DependencyCycleContext, DependencyCycleError, ErrorKind,
    ErrorSeverity, HeartbeatTimeoutContext, HeartbeatTimeoutError, LockTimeoutContext,
    LockTimeoutError, NamespaceViolationContext, NamespaceViolationError, NexusError, NexusResult,
    ResultExt, SlotId,
};

pub use ring_buffer::{BoundedQueue, Event, MpmcRingBuffer, SpscRingBuffer, TypedRingBuffer};

pub use rwlock::{
    FairRwLock, InstrumentedRwLock, ReaderPriorityRwLock, RwLock, RwLockExt, RwLockReadGuard,
    RwLockWriteGuard, TimedRwLock, TimedWriteGuard, UpgradableReadGuard, UpgradableRwLock,
};

pub use timing::{
    AdaptiveTimer, Deadline, DriftCorrectedTimer, HeartbeatTimer, Phase, RateLimiter, Timestamp,
    NANOS_PER_HOUR, NANOS_PER_MICRO, NANOS_PER_MILLI, NANOS_PER_SEC,
};

pub use dash_map::{DashMap, DashSet};
// ============================================================================
// Prelude
// ============================================================================

/// A prelude for commonly used types.
///
/// This module contains the most commonly used types and traits from this crate.
pub mod prelude {
    pub use crate::atomic::{AtomicTimestamp, AtomicCounter};
    pub use crate::error::{NexusError, NexusResult, ErrorKind};
    pub use crate::{bail, ensure};
    pub use crate::timing::{Deadline, HeartbeatTimer, Timestamp};
    pub use crate::rwlock::RwLock;
    pub use crate::ring_buffer::MpmcRingBuffer;
    pub use crate::dash_map::DashMap;
}

// ============================================================================
// Type Aliases
// ============================================================================

/// A type alias for the standard Result type using NexusError.
pub type Result<T> = std::result::Result<T, NexusError>;

/// Type alias for a Lamport timestamp (used in event bus for causal ordering).
pub type LamportTimestamp = u64;

/// Type alias for a goal ID (used throughout NEXUS).
pub type GoalId = u64;

/// Type alias for an agent ID.
pub type AgentId = u64;

/// Type alias for a namespace ID (used in typed buffers).
pub type NamespaceId = u64;

// ============================================================================
// Constants
// ============================================================================

/// Default cache line size (64 bytes on modern CPUs).
pub const CACHE_LINE_SIZE: usize = 64;

/// Default number of shards for concurrent maps.
pub const DEFAULT_SHARD_COUNT: usize = 64;

/// Maximum number of entries in a ring buffer.
pub const MAX_RING_BUFFER_CAPACITY: usize = 16_777_216; // 16M entries

/// Maximum number of retries for CAS operations.
pub const MAX_CAS_RETRIES: usize = 128;

/// Default timeout for lock acquisition (in milliseconds).
pub const DEFAULT_LOCK_TIMEOUT_MS: u64 = 5000;

/// Maximum wait time for queue operations (in milliseconds).
pub const MAX_QUEUE_WAIT_MS: u64 = 30_000;

// ============================================================================
// Utility Functions
// ============================================================================

/// Returns the current timestamp as nanoseconds since Unix epoch.
///
/// This is a convenience wrapper around `timing::system_time::now_nanos()`.
#[inline]
pub fn now_nanos() -> u64 {
    timing::system_time::now_nanos()
}

/// Returns the current timestamp as milliseconds since Unix epoch.
///
/// This is a convenience wrapper around `timing::system_time::now_millis()`.
#[inline]
pub fn now_millis() -> u64 {
    timing::system_time::now_millis()
}

/// Returns the current timestamp as seconds since Unix epoch.
///
/// This is a convenience wrapper around `timing::system_time::now_secs()`.
#[inline]
pub fn now_secs() -> u64 {
    timing::system_time::now_secs()
}

/// Returns a monotonic timestamp for use in Lamport clocks.
///
/// This guarantees monotonicity - the timestamp never goes backwards
/// even if the system clock is adjusted.
#[inline]
pub fn monotonic_timestamp() -> Timestamp {
    Timestamp::now()
}

/// Calculates the next power of 2 greater than or equal to n.
///
/// This is used for sizing ring buffers and hash tables.
#[inline]
pub fn next_power_of_2(n: usize) -> usize {
    n.next_power_of_two()
}

/// Checks if a value is a power of 2.
#[inline]
pub fn is_power_of_2(n: usize) -> bool {
    n > 0 && (n & (n - 1)) == 0
}

/// Aligns a value up to the given alignment.
///
/// The alignment must be a power of 2.
#[inline]
pub const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

/// Aligns a value down to the given alignment.
///
/// The alignment must be a power of 2.
#[inline]
pub const fn align_down(value: usize, alignment: usize) -> usize {
    value & !(alignment - 1)
}

// ============================================================================
// Documentation Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_monotonic_timestamp() {
        let t1 = monotonic_timestamp();
        let t2 = monotonic_timestamp();
        assert!(t2.nanos >= t1.nanos);
    }

    #[test]
    fn test_next_power_of_2() {
        assert_eq!(next_power_of_2(0), 1);
        assert_eq!(next_power_of_2(1), 1);
        assert_eq!(next_power_of_2(2), 2);
        assert_eq!(next_power_of_2(3), 4);
        assert_eq!(next_power_of_2(100), 128);
        assert_eq!(next_power_of_2(1024), 1024);
    }

    #[test]
    fn test_is_power_of_2() {
        assert!(!is_power_of_2(0));
        assert!(is_power_of_2(1));
        assert!(is_power_of_2(2));
        assert!(is_power_of_2(4));
        assert!(is_power_of_2(64));
        assert!(!is_power_of_2(3));
        assert!(!is_power_of_2(100));
    }

    #[test]
    fn test_align_up_down() {
        assert_eq!(align_up(100, 64), 128);
        assert_eq!(align_up(64, 64), 64);
        assert_eq!(align_up(0, 64), 0);
        assert_eq!(align_down(100, 64), 64);
        assert_eq!(align_down(128, 64), 128);
    }

    #[test]
    fn test_concurrent_atomic_timestamp() {
        let ts = Arc::new(AtomicTimestamp::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let ts_clone = ts.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    ts_clone.advance_by(1);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(ts.load(Ordering::Relaxed), 10000);
    }

    #[test]
    fn test_concurrent_counter() {
        let counter = Arc::new(AtomicCounter::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let counter_clone = counter.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    counter_clone.increment(Ordering::Relaxed);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(counter.load(Ordering::Relaxed), 10000);
    }

    #[test]
    fn test_prelude_exports() {
        use prelude::*;

        let ts = AtomicTimestamp::new(0);
        assert_eq!(ts.load(Ordering::Relaxed), 0);

        let counter = AtomicCounter::new(0);
        counter.increment(Ordering::Relaxed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_time_functions() {
        let nanos = now_nanos();
        let millis = now_millis();
        let secs = now_secs();

        assert!(nanos > 0);
        assert!(millis > 0);
        assert!(secs > 0);

        // Verify consistency
        assert!(nanos / 1_000_000 >= millis.saturating_sub(1));
    }

    #[test]
    fn test_type_aliases() {
        let _goal_id: GoalId = 42;
        let _agent_id: AgentId = 123;
        let _namespace_id: NamespaceId = 456;
        let _lamport_ts: LamportTimestamp = 789;
    }
}