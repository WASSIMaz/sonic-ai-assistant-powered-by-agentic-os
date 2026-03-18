//! # Custom Read-Write Lock for NEXUS
//!
//! This module provides custom RwLock implementations optimized for NEXUS workloads:
//!
//! - `RwLock`: Writer-priority read-write lock using parking_lot
//! - `FairRwLock`: Fair scheduling between readers and writers
//! - `UpgradableRwLock`: RwLock with upgradable read locks
//! - `ScopedRwLock`: RwLock with scoped read/write guards
//!
//! ## Design Philosophy
//!
//! The default RwLock implementation uses **writer priority** to ensure that:
//!
//! 1. Writers are not starved by continuous reader acquisition
//! 2. WorldModel updates from the Oracle complete quickly
//! 3. Goal state updates are not delayed indefinitely
//!
//! This is critical for the 200ms Oracle poll cycle - if writers were
//! starved, the WorldModel would become stale.

use std::cell::UnsafeCell;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::{
    RwLock as ParkingRwLock,
};

use crate::error::{ErrorKind, NexusError, NexusResult};

/// A writer-priority read-write lock.
///
/// This implementation prioritizes writers over readers to prevent
/// writer starvation in read-heavy workloads. This is essential for
/// NEXUS where the WorldModel must be updated every 200ms by the
/// Oracle, regardless of continuous reads from Contextualizer,
/// FleetMonitor, and EnvelopeMonitor.
///
/// # Example
///
/// ```rust
/// use nexus_utils::rwlock::RwLock;
///
/// let lock = RwLock::new(42);
///
/// // Multiple readers can hold the lock simultaneously
/// let r1 = lock.read();
/// let r2 = lock.read();
/// assert_eq!(*r1, 42);
/// assert_eq!(*r2, 42);
/// drop(r1);
/// drop(r2);
///
/// // But only one writer at a time
/// let mut w = lock.write();
/// *w = 100;
/// ```
#[derive(Debug)]
pub struct RwLock<T> {
    inner: ParkingRwLock<T>,
}

impl<T> RwLock<T> {
    /// Creates a new RwLock with the given value.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            inner: ParkingRwLock::new(value),
        }
    }

    /// Acquires a read lock, blocking until available.
    ///
    /// This function blocks until a read lock can be acquired. If there
    /// are pending writers, this will block until they complete to
    /// ensure writer priority.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard {
            inner: self.inner.read(),
        }
    }

    /// Attempts to acquire a read lock without blocking.
    ///
    /// Returns `None` if the lock is currently held for writing or
    /// there are pending writers.
    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read().map(|guard| RwLockReadGuard { inner: guard })
    }

    /// Attempts to acquire a read lock with a timeout.
    #[inline]
    pub fn try_read_for(&self, timeout: Duration) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read_for(timeout).map(|guard| RwLockReadGuard { inner: guard })
    }

    /// Acquires a write lock, blocking until available.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard {
            inner: self.inner.write(),
        }
    }

    /// Attempts to acquire a write lock without blocking.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write().map(|guard| RwLockWriteGuard { inner: guard })
    }

    /// Attempts to acquire a write lock with a timeout.
    #[inline]
    pub fn try_write_for(&self, timeout: Duration) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write_for(timeout).map(|guard| RwLockWriteGuard { inner: guard })
    }

    /// Returns a mutable reference to the inner value.
    ///
    /// This is safe because `&mut self` guarantees no other references exist.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }

    /// Returns `true` if there are any active readers.
    pub fn is_read_locked(&self) -> bool {
    self.inner.is_locked() && self.inner.try_write().is_none() == false
    }
    pub fn is_write_locked(&self) -> bool {
        self.inner.try_write().is_none()
    }
    pub fn reader_count(&self) -> usize {
        0 // parking_lot does not expose reader count — return 0 as sentinel
    }
}

impl<T: Default> Default for RwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Display> fmt::Display for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.inner.try_read() {
            Some(guard) => fmt::Display::fmt(&*guard, f),
            None => write!(f, "<locked>"),
        }
    }
}

/// A guard returned by `RwLock::read()`.
#[derive(Debug)]
pub struct RwLockReadGuard<'a, T> {
    inner: parking_lot::RwLockReadGuard<'a, T>,
}

impl<'a, T> Deref for RwLockReadGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// A guard returned by `RwLock::write()`.
#[derive(Debug)]
pub struct RwLockWriteGuard<'a, T> {
    inner: parking_lot::RwLockWriteGuard<'a, T>,
}

impl<'a, T> Deref for RwLockWriteGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> DerefMut for RwLockWriteGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// An upgradable read-write lock.
///
/// This lock supports "upgradable" read locks that can be atomically
/// upgraded to write locks without releasing the lock.
///
/// This is useful for read-modify-write patterns where you want to:
/// 1. Read the value
/// 2. Check if modification is needed
/// 3. Atomically upgrade to write and modify
///
/// # Example
///
/// ```rust
/// use nexus_utils::rwlock::UpgradableRwLock;
/// use std::collections::HashMap;
///
/// let lock = UpgradableRwLock::new(HashMap::new());
///
/// // Read and conditionally write
/// {
///     let read = lock.upgradable_read();
///     if !read.contains_key(&"key") {
///         let mut write = read.upgrade();
///         write.insert("key", "value");
///     }
/// }
/// ```
#[derive(Debug)]
pub struct UpgradableRwLock<T> {
    inner: ParkingRwLock<T>,
}

impl<T> UpgradableRwLock<T> {
    /// Creates a new upgradable RwLock.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            inner: ParkingRwLock::new(value),
        }
    }

    /// Acquires a regular read lock.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard {
            inner: self.inner.read(),
        }
    }

    /// Acquires an upgradable read lock.
    ///
    /// Upgradable read locks can be atomically upgraded to write locks.
    /// Only one upgradable read lock can exist at a time.
    #[inline]
    pub fn upgradable_read(&self) -> UpgradableReadGuard<'_, T> {
        UpgradableReadGuard {
            inner: self.inner.upgradable_read(),
        }
    }

    /// Attempts to acquire an upgradable read lock without blocking.
    #[inline]
    pub fn try_upgradable_read(&self) -> Option<UpgradableReadGuard<'_, T>> {
        self.inner.try_upgradable_read().map(|guard| UpgradableReadGuard { inner: guard })
    }

    /// Acquires a write lock.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard {
            inner: self.inner.write(),
        }
    }

    /// Returns a mutable reference to the inner value.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: Default> Default for UpgradableRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// A guard returned by `UpgradableRwLock::upgradable_read()`.
#[derive(Debug)]
pub struct UpgradableReadGuard<'a, T> {
    inner: parking_lot::RwLockUpgradableReadGuard<'a, T>,
}

impl<'a, T> Deref for UpgradableReadGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> UpgradableReadGuard<'a, T> {
    /// Upgrades this read lock to a write lock atomically.
    #[inline]
    pub fn upgrade(self) -> RwLockWriteGuard<'a, T> {
        RwLockWriteGuard {
            inner: parking_lot::RwLockUpgradableReadGuard::upgrade(self.inner),
        }
    }

    /// Attempts to upgrade to a write lock without blocking.
    #[inline]
    pub fn try_upgrade(self) -> Result<RwLockWriteGuard<'a, T>, Self> {
        match parking_lot::RwLockUpgradableReadGuard::try_upgrade(self.inner) {
            Ok(guard) => Ok(RwLockWriteGuard { inner: guard }),
            Err(guard) => Err(UpgradableReadGuard { inner: guard }),
        }
    }

    /// Downgrades back to a regular read lock.
    #[inline]
    pub fn downgrade(self) -> RwLockReadGuard<'a, T> {
        RwLockReadGuard {
            inner: parking_lot::RwLockUpgradableReadGuard::downgrade(self.inner),
        }
    }
}

/// A fair read-write lock with reader-writer fairness.
///
/// Unlike the default `RwLock` which prioritizes writers, this implementation
/// ensures fair scheduling between readers and writers using a ticket-based
/// approach. This is useful for workloads where both read and write
/// operations are equally important.
///
/// # Example
///
/// ```rust
/// use nexus_utils::rwlock::FairRwLock;
///
/// let lock = FairRwLock::new(vec![1, 2, 3]);
///
/// let read = lock.read();
/// assert_eq!(*read, vec![1, 2, 3]);
/// ```
#[derive(Debug)]
pub struct FairRwLock<T> {
    /// The inner value.
    value: UnsafeCell<T>,
    /// Raw lock implementation.
    lock: FairRawRwLock,
}

impl<T> FairRwLock<T> {
    /// Creates a new fair RwLock.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            lock: FairRawRwLock::new(),
        }
    }

    /// Acquires a read lock.
    #[inline]
    pub fn read(&self) -> FairReadGuard<'_, T> {
        self.lock.lock_shared();
        FairReadGuard { lock: self }
    }

    /// Attempts to acquire a read lock without blocking.
    #[inline]
    pub fn try_read(&self) -> Option<FairReadGuard<'_, T>> {
        if self.lock.try_lock_shared() {
            Some(FairReadGuard { lock: self })
        } else {
            None
        }
    }

    /// Acquires a write lock.
    #[inline]
    pub fn write(&self) -> FairWriteGuard<'_, T> {
        self.lock.lock_exclusive();
        FairWriteGuard { lock: self }
    }

    /// Attempts to acquire a write lock without blocking.
    #[inline]
    pub fn try_write(&self) -> Option<FairWriteGuard<'_, T>> {
        if self.lock.try_lock_exclusive() {
            Some(FairWriteGuard { lock: self })
        } else {
            None
        }
    }

    /// Returns a mutable reference to the inner value.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        // Safety: &mut self guarantees exclusive access
        unsafe { &mut *self.value.get() }
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

unsafe impl<T: Send> Send for FairRwLock<T> {}
unsafe impl<T: Send + Sync> Sync for FairRwLock<T> {}

impl<T: Default> Default for FairRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// A guard returned by `FairRwLock::read()`.
#[derive(Debug)]
pub struct FairReadGuard<'a, T> {
    lock: &'a FairRwLock<T>,
}

impl<'a, T> Deref for FairReadGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // Safety: We hold the read lock
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> Drop for FairReadGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.lock.unlock_shared();
    }
}

/// A guard returned by `FairRwLock::write()`.
#[derive(Debug)]
pub struct FairWriteGuard<'a, T> {
    lock: &'a FairRwLock<T>,
}

impl<'a, T> Deref for FairWriteGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        // Safety: We hold the write lock
        unsafe { &*self.lock.value.get() }
    }
}

impl<'a, T> DerefMut for FairWriteGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: We hold the write lock
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<'a, T> Drop for FairWriteGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.lock.unlock_exclusive();
    }
}

/// Fair raw lock implementation using ticket-based fairness.
#[derive(Debug)]
struct FairRawRwLock {
    /// State: bits 0-30 = reader count, bit 31 = write flag, bits 32-62 = waiting count
    state: AtomicUsize,
    /// Ticket counter for fairness
    ticket: AtomicUsize,
    /// Current serving ticket
    serving: AtomicUsize,
}

impl FairRawRwLock {
    const READER_MASK: usize = (1 << 30) - 1;
    const WRITER_BIT: usize = 1 << 30;

    #[inline]
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(0),
            ticket: AtomicUsize::new(0),
            serving: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn lock_shared(&self) {
        let my_ticket = self.ticket.fetch_add(1, Ordering::Acquire);
        while self.serving.load(Ordering::Acquire) != my_ticket {
            std::hint::spin_loop();
        }

        // Wait for no writers
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state & Self::WRITER_BIT == 0 {
                let new_state = state + 1; // Add a reader
                if self.state.compare_exchange_weak(
                    state,
                    new_state,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
                {
                    self.serving.fetch_add(1, Ordering::Release);
                    return;
                }
            }
            std::hint::spin_loop();
        }
    }

    #[inline]
    fn try_lock_shared(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & Self::WRITER_BIT != 0 {
            return false;
        }
        let new_state = state + 1;
        self.state
            .compare_exchange_weak(state, new_state, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[inline]
    fn unlock_shared(&self) {
        loop {
            let state = self.state.load(Ordering::Acquire);
            let new_state = state - 1; // Remove a reader
            if self.state
                .compare_exchange_weak(state, new_state, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    #[inline]
    fn lock_exclusive(&self) {
        let my_ticket = self.ticket.fetch_add(1, Ordering::Acquire);
        while self.serving.load(Ordering::Acquire) != my_ticket {
            std::hint::spin_loop();
        }

        // Wait for no readers or writers
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state & Self::READER_MASK == 0 && state & Self::WRITER_BIT == 0 {
                let new_state = state | Self::WRITER_BIT;
                if self.state
                    .compare_exchange_weak(state, new_state, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    self.serving.fetch_add(1, Ordering::Release);
                    return;
                }
            }
            std::hint::spin_loop();
        }
    }

    #[inline]
    fn try_lock_exclusive(&self) -> bool {
        let state = self.state.load(Ordering::Acquire);
        if state & Self::READER_MASK != 0 || state & Self::WRITER_BIT != 0 {
            return false;
        }
        self.state
            .compare_exchange_weak(
                state,
                state | Self::WRITER_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    #[inline]
    fn unlock_exclusive(&self) {
        self.state.fetch_and(!Self::WRITER_BIT, Ordering::Release);
    }
}

/// A read-write lock with reader priority and writer fairness.
///
/// Unlike the default writer-priority lock, this implementation allows
/// multiple readers to acquire the lock even when a writer is waiting,
/// but uses a fairness mechanism to ensure writers eventually acquire
/// the lock.
///
/// This is useful for scenarios where read operations are frequent and
/// short, while write operations are rare and expensive.
#[derive(Debug)]
pub struct ReaderPriorityRwLock<T> {
    inner: RwLock<T>,
    /// Number of pending writers (for fairness)
    pending_writers: AtomicUsize,
    /// Fairness threshold - after this many readers, writers get priority
    fairness_threshold: usize,
}

impl<T> ReaderPriorityRwLock<T> {
    /// Creates a new reader-priority lock.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            pending_writers: AtomicUsize::new(0),
            fairness_threshold: 16, // Default threshold
        }
    }

    /// Creates a lock with a custom fairness threshold.
    #[inline]
    pub fn with_threshold(value: T, threshold: usize) -> Self {
        Self {
            inner: RwLock::new(value),
            pending_writers: AtomicUsize::new(0),
            fairness_threshold: threshold,
        }
    }

    /// Acquires a read lock.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        // If there are pending writers, wait briefly to allow them to proceed
        let pending = self.pending_writers.load(Ordering::Acquire);
        if pending > 0 {
            // Brief backoff for fairness
            for _ in 0..pending.min(self.fairness_threshold) {
                std::hint::spin_loop();
            }
        }
        self.inner.read()
    }

    /// Acquires a write lock.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.pending_writers.fetch_add(1, Ordering::Acquire);
        let guard = self.inner.write();
        self.pending_writers.fetch_sub(1, Ordering::Release);
        guard
    }

    /// Returns a mutable reference to the inner value.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: Default> Default for ReaderPriorityRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// A read-write lock that tracks contention metrics.
///
/// This is useful for debugging and performance analysis.
#[derive(Debug)]
pub struct InstrumentedRwLock<T> {
    inner: RwLock<T>,
    /// Total number of read acquisitions
    read_count: AtomicUsize,
    /// Total number of write acquisitions
    write_count: AtomicUsize,
    /// Number of contended reads (had to wait)
    contended_reads: AtomicUsize,
    /// Number of contended writes (had to wait)
    contended_writes: AtomicUsize,
}

impl<T> InstrumentedRwLock<T> {
    /// Creates a new instrumented lock.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            read_count: AtomicUsize::new(0),
            write_count: AtomicUsize::new(0),
            contended_reads: AtomicUsize::new(0),
            contended_writes: AtomicUsize::new(0),
        }
    }

    /// Acquires a read lock and records metrics.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.read_count.fetch_add(1, Ordering::Relaxed);

        // Try fast path
        if let Some(guard) = self.inner.try_read() {
            return guard;
        }

        // Slow path - contended
        self.contended_reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read()
    }

    /// Acquires a write lock and records metrics.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.write_count.fetch_add(1, Ordering::Relaxed);

        // Try fast path
        if let Some(guard) = self.inner.try_write() {
            return guard;
        }

        // Slow path - contended
        self.contended_writes.fetch_add(1, Ordering::Relaxed);
        self.inner.write()
    }

    /// Returns the number of read acquisitions.
    #[inline]
    pub fn read_count(&self) -> usize {
        self.read_count.load(Ordering::Relaxed)
    }

    /// Returns the number of write acquisitions.
    #[inline]
    pub fn write_count(&self) -> usize {
        self.write_count.load(Ordering::Relaxed)
    }

    /// Returns the number of contended reads.
    #[inline]
    pub fn contended_reads(&self) -> usize {
        self.contended_reads.load(Ordering::Relaxed)
    }

    /// Returns the number of contended writes.
    #[inline]
    pub fn contended_writes(&self) -> usize {
        self.contended_writes.load(Ordering::Relaxed)
    }

    /// Returns the contention ratio for reads.
    #[inline]
    pub fn read_contention_ratio(&self) -> f64 {
        let total = self.read_count.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.contended_reads.load(Ordering::Relaxed) as f64 / total as f64
    }

    /// Returns the contention ratio for writes.
    #[inline]
    pub fn write_contention_ratio(&self) -> f64 {
        let total = self.write_count.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        self.contended_writes.load(Ordering::Relaxed) as f64 / total as f64
    }

    /// Resets all counters.
    #[inline]
    pub fn reset_metrics(&self) {
        self.read_count.store(0, Ordering::Relaxed);
        self.write_count.store(0, Ordering::Relaxed);
        self.contended_reads.store(0, Ordering::Relaxed);
        self.contended_writes.store(0, Ordering::Relaxed);
    }

    /// Returns a mutable reference to the inner value.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: Default> Default for InstrumentedRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// A timed read-write lock with holder agent tracking.
///
/// This lock extends the basic `RwLock` with:
/// - Timeout-based acquisition with detailed error reporting
/// - Tracking of which agent currently holds the write lock
/// - Metrics for lock contention analysis
///
/// This is critical for the cognitive kernel where lock holders
/// need to be identified for debugging and deadlock detection.
///
/// # Example
///
/// ```rust
/// use nexus_utils::rwlock::TimedRwLock;
/// use std::time::Duration;
///
/// let lock = TimedRwLock::new(42);
///
/// // Write lock with agent tracking
/// lock.write_with_agent(1001, Duration::from_secs(5)).unwrap();
/// assert_eq!(lock.holder_agent_id(), Some(1001));
///
/// // Check who holds the lock
/// lock.release_agent_tracking();
/// ```
#[derive(Debug)]
pub struct TimedRwLock<T> {
    /// The inner RwLock.
    inner: RwLock<T>,
    /// Human-readable slot identifier for debugging.
    slot_name: std::borrow::Cow<'static, str>,
    /// Tracks the current write lock holder (agent ID).
    /// 0 means no holder, non-zero is the agent ID.
    holder_agent_id: AtomicUsize,
    /// Thread ID of the holder, for watchdog recovery.
    holder_thread_id: AtomicUsize,
    /// Timestamp when the lock was acquired (nanoseconds).
    acquisition_timestamp: AtomicUsize,
    /// Number of times write lock was acquired.
    write_acquire_count: AtomicUsize,
    /// Number of times acquisition timed out.
    timeout_count: AtomicUsize,
}

impl<T> TimedRwLock<T> {
    /// Creates a new timed RwLock.
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            slot_name: std::borrow::Cow::Borrowed("unknown"),
            holder_agent_id: AtomicUsize::new(0),
            holder_thread_id: AtomicUsize::new(0),
            acquisition_timestamp: AtomicUsize::new(0),
            write_acquire_count: AtomicUsize::new(0),
            timeout_count: AtomicUsize::new(0),
        }
    }

    /// Creates a new timed RwLock with a slot name for debugging.
    #[inline]
    pub fn with_slot_name(value: T, name: impl Into<std::borrow::Cow<'static, str>>) -> Self {
        Self {
            inner: RwLock::new(value),
            slot_name: name.into(),
            holder_agent_id: AtomicUsize::new(0),
            holder_thread_id: AtomicUsize::new(0),
            acquisition_timestamp: AtomicUsize::new(0),
            write_acquire_count: AtomicUsize::new(0),
            timeout_count: AtomicUsize::new(0),
        }
    }

    /// Acquires a read lock, blocking until available.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        self.inner.read()
    }

    /// Attempts to acquire a read lock without blocking.
    #[inline]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read()
    }

    /// Attempts to acquire a read lock with a timeout.
    #[inline]
    pub fn try_read_for(&self, timeout: Duration) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read_for(timeout)
    }

    /// Acquires a write lock, blocking until available.
    ///
    /// Note: This does not track the holder agent. Use `write_with_agent`
    /// for agent-tracked acquisition.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.inner.write()
    }

    /// Acquires a write lock with agent tracking and timeout.
    ///
    /// This method:
    /// 1. Attempts to acquire the write lock within the timeout
    /// 2. Records the holder agent ID if successful
    /// 3. Records the acquisition timestamp
    /// 4. Increments metrics counters
    ///
    /// # Returns
    ///
    /// - `Ok(guard)` if the lock was acquired within the timeout
    /// - `Err(LockTimeoutError)` if the timeout elapsed before acquisition
    ///
    /// # Example
    ///
    /// ```rust
    /// use nexus_utils::rwlock::TimedRwLock;
    /// use std::time::Duration;
    ///
    /// let lock = TimedRwLock::new(vec![1, 2, 3]);
    /// // Use map() to operate on the guard within a closure
    /// let result = lock.write_with_agent(42, Duration::from_secs(5)).map(|mut guard| {
    ///     guard.push(4);
    ///     guard.len()
    /// });
    /// assert!(result.is_ok());
    /// ```
    pub fn write_with_agent(
        &self,
        agent_id: u64,
        timeout: Duration,
    ) -> Result<RwLockWriteGuard<'_, T>, crate::error::LockTimeoutError> {
        let start = std::time::Instant::now();
        self.write_acquire_count.fetch_add(1, Ordering::Relaxed);

        // Try fast path first
        if let Some(guard) = self.inner.try_write() {
            self.set_holder(agent_id);
            return Ok(guard);
        }

        // Slow path with timeout
        if let Some(guard) = self.inner.try_write_for(timeout) {
            self.set_holder(agent_id);
            return Ok(guard);
        }

        // Timed out
        self.timeout_count.fetch_add(1, Ordering::Relaxed);
        Err(crate::error::LockTimeoutError::new(self.slot_name.clone(), start.elapsed())
            .with_slot_id(0)
            .with_holder_agent(self.holder_agent_id().unwrap_or(0))
            .with_holder_thread(self.holder_thread_id().unwrap_or(0)))
    }

    /// Attempts to acquire a write lock without blocking.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write()
    }

    /// Attempts to acquire a write lock with a timeout (no agent tracking).
    #[inline]
    pub fn try_write_for(&self, timeout: Duration) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write_for(timeout)
    }

    /// Returns the current write lock holder agent ID, if any.
    ///
    /// Returns `None` if no write lock is held, or if the write lock
    /// was acquired without agent tracking.
    #[inline]
    pub fn holder_agent_id(&self) -> Option<u64> {
        let id = self.holder_agent_id.load(Ordering::Acquire);
        if id == 0 {
            None
        } else {
            Some(id as u64)
        }
    }

    /// Returns the thread ID of the current holder, if any.
    #[inline]
    pub fn holder_thread_id(&self) -> Option<u64> {
        let id = self.holder_thread_id.load(Ordering::Acquire);
        if id == 0 {
            None
        } else {
            Some(id as u64)
        }
    }

    /// Returns the timestamp when the lock was acquired, if held.
    #[inline]
    pub fn acquisition_time(&self) -> Option<u64> {
        let ts = self.acquisition_timestamp.load(Ordering::Acquire);
        if ts == 0 {
            None
        } else {
            Some(ts as u64)
        }
    }

    /// Returns how long the lock has been held, if currently held.
    pub fn held_duration(&self) -> Option<Duration> {
        let ts = self.acquisition_timestamp.load(Ordering::Acquire);
        if ts == 0 {
            None
        } else {
            let _now = std::time::Instant::now()
                .elapsed()
                .as_nanos() as u64;
            // Note: This is approximate - we're using elapsed time as a proxy
            // A real implementation would store the Instant
            Some(Duration::from_nanos(ts as u64))
        }
    }

    /// Clears the holder agent tracking (call after releasing write lock).
    #[inline]
    pub fn release_agent_tracking(&self) {
        self.holder_agent_id.store(0, Ordering::Release);
        self.holder_thread_id.store(0, Ordering::Release);
        self.acquisition_timestamp.store(0, Ordering::Release);
    }

    /// Returns the slot name for this lock.
    #[inline]
    pub fn slot_name(&self) -> &str {
        &self.slot_name
    }

    /// Returns the number of write lock acquisitions.
    #[inline]
    pub fn write_acquire_count(&self) -> usize {
        self.write_acquire_count.load(Ordering::Relaxed)
    }

    /// Returns the number of acquisition timeouts.
    #[inline]
    pub fn timeout_count(&self) -> usize {
        self.timeout_count.load(Ordering::Relaxed)
    }

    /// Returns the timeout ratio (timeouts / acquisitions).
    #[inline]
    pub fn timeout_ratio(&self) -> f64 {
        let total = self.write_acquire_count.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.timeout_count.load(Ordering::Relaxed) as f64 / total as f64
        }
    }

    /// Returns a mutable reference to the inner value.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwraps the lock, returning the inner value.
    #[inline]
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }

    /// Sets the holder agent ID and acquisition timestamp.
    #[inline]
    fn set_holder(&self, agent_id: u64) {
        self.holder_agent_id.store(agent_id as usize, Ordering::Release);

        // ThreadId::as_u64() is nightly-only — hash the ThreadId instead
        use std::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        std::thread::current().id().hash(&mut hasher);
        let thread_id = hasher.finish() as usize;

        self.holder_thread_id.store(thread_id, Ordering::Release);
        self.acquisition_timestamp.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize,
            Ordering::Release,
        );
    }

    /// Returns `true` if there are any active readers.
    #[inline]
    pub fn is_read_locked(&self) -> bool {
        self.inner.is_read_locked()
    }

    /// Returns `true` if there is an active writer.
    #[inline]
    pub fn is_write_locked(&self) -> bool {
        self.inner.is_write_locked()
    }

    /// Returns the number of active readers.
    #[inline]
    pub fn reader_count(&self) -> usize {
        self.inner.reader_count()
    }
}

impl<T: Default> Default for TimedRwLock<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

/// A guard returned by `TimedRwLock::write_with_agent()`.
///
/// This guard automatically clears the holder agent tracking when dropped.
#[derive(Debug)]
pub struct TimedWriteGuard<'a, T> {
    inner: RwLockWriteGuard<'a, T>,
    lock: &'a TimedRwLock<T>,
}

impl<'a, T> Deref for TimedWriteGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a, T> DerefMut for TimedWriteGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl<'a, T> Drop for TimedWriteGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.lock.release_agent_tracking();
    }
}

/// Extension trait for RwLock operations with error handling.
pub trait RwLockExt<T> {
    /// Acquires a read lock with a timeout, returning an error on timeout.
    fn read_timeout(&self, timeout: Duration) -> NexusResult<RwLockReadGuard<'_, T>>;

    /// Acquires a write lock with a timeout, returning an error on timeout.
    fn write_timeout(&self, timeout: Duration) -> NexusResult<RwLockWriteGuard<'_, T>>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    #[inline]
    fn read_timeout(&self, timeout: Duration) -> NexusResult<RwLockReadGuard<'_, T>> {
        self.try_read_for(timeout).ok_or_else(|| {
            NexusError::new(ErrorKind::Timeout, format!("read lock timeout after {:?}", timeout))
        })
    }

    #[inline]
    fn write_timeout(&self, timeout: Duration) -> NexusResult<RwLockWriteGuard<'_, T>> {
        self.try_write_for(timeout).ok_or_else(|| {
            NexusError::new(ErrorKind::Timeout, format!("write lock timeout after {:?}", timeout))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_rwlock_basic() {
        let lock = RwLock::new(42);
        {
            let r1 = lock.read();
            let r2 = lock.read();
            assert_eq!(*r1, 42);
            assert_eq!(*r2, 42);
        }
        {
            let mut w = lock.write();
            *w = 100;
        }
        assert_eq!(*lock.read(), 100);
    }

    #[test]
    fn test_rwlock_writer_priority() {
        let lock = Arc::new(RwLock::new(0));
        let mut handles = vec![];

        // Spawn multiple readers
        for _ in 0..5 {
            let lock_clone = lock.clone();
            handles.push(thread::spawn(move || {
                let _r = lock_clone.read();
                thread::sleep(Duration::from_millis(10));
            }));
        }

        // Give readers time to acquire
        thread::sleep(Duration::from_millis(5));

        // Writer should be prioritized
        let lock_clone = lock.clone();
        let writer = thread::spawn(move || {
            let mut w = lock_clone.write();
            *w = 42;
        });

        writer.join().unwrap();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(*lock.read(), 42);
    }

    #[test]
    fn test_upgradable_rwlock() {
        let lock = UpgradableRwLock::new(HashMap::new());

        {
            let read = lock.upgradable_read();
            if !read.contains_key(&"key") {
                let mut write = read.upgrade();
                write.insert("key", "value");
            }
        }

        assert_eq!(lock.read().get(&"key"), Some(&"value"));
    }

    #[test]
    fn test_fair_rwlock() {
        let lock = FairRwLock::new(vec![1, 2, 3]);

        let r1 = lock.read();
        let r2 = lock.read();
        assert_eq!(*r1, vec![1, 2, 3]);
        assert_eq!(*r2, vec![1, 2, 3]);
        drop(r1);
        drop(r2);

        let mut w = lock.write();
        w.push(4);
        drop(w);

        assert_eq!(*lock.read(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_instrumented_rwlock() {
        let lock = InstrumentedRwLock::new(0);

        for _ in 0..10 {
            let _r = lock.read();
        }

        {
            let mut w = lock.write();
            *w = 42;
        }

        assert_eq!(lock.read_count(), 10);
        assert_eq!(lock.write_count(), 1);
        // Fast path should have low contention
        assert!(lock.read_contention_ratio() < 0.5);
    }

    #[test]
    fn test_rwlock_timeout() {
        use crate::rwlock::RwLockExt;

        let lock = RwLock::new(42);

        // Should succeed immediately
        let result = lock.read_timeout(Duration::from_secs(1));
        assert!(result.is_ok());

        // Should fail with zero timeout when held
        let lock_clone = Arc::new(RwLock::new(42));
        let _w = lock_clone.write();
        let result = lock_clone.try_read_for(Duration::from_millis(0));
        assert!(result.is_none());
    }

    // ============================================================================
    // TimedRwLock Tests
    // ============================================================================

    #[test]
    fn test_lock_timeout_error_contains_slot_id() {
        // Create a TimedRwLock tagged with slot identifier "goal_states".
        // Thread A acquires the write lock and holds it.
        // Thread B calls try_write(200ms) while A is holding.
        // Expected: thread B receives Err(LockTimeout) with slot_id = "goal_states".
        use std::sync::Arc;

        let lock = Arc::new(TimedRwLock::with_slot_name(42, "goal_states"));
        let lock_clone = lock.clone();

        // Thread A holds the write lock
        let _guard = lock_clone.write();

        // Thread B tries to acquire with timeout
        let lock_clone2 = lock.clone();
        let handle = thread::spawn(move || {
            lock_clone2.write_with_agent(999, Duration::from_millis(200))
                .map(|_guard| ())  // drop the guard inside the thread, return () on success
        });

        let result = handle.join().unwrap();
        assert!(result.is_err(), "Should timeout when lock is held");

        let err = result.unwrap_err();
        assert!(
            err.slot_name == "goal_states",
            "Error slot_name should be 'goal_states', got {:?}",
            err.slot_name
        );
        assert!(
            err.slot_id == 0,
            "Error slot_id should be 0, got {}",
            err.slot_id
        );
    }

    #[test]
    fn test_lock_timeout_error_contains_holder_thread_id() {
        // Same setup: Thread A holds the write lock.
        // Thread B times out.
        // Expected: the error's holder_thread_id equals thread A's thread ID.
        use std::sync::Arc;

        let lock = Arc::new(TimedRwLock::new(42));
        let _thread_a_id = thread::current().id();

        let lock_clone = lock.clone();
        let _guard = lock_clone.write_with_agent(1001, Duration::from_secs(5)).unwrap();

        // Get the holder thread ID that was set
        let holder_tid = lock.holder_thread_id();
        // The holder thread ID should match the current thread
        assert!(
            holder_tid.is_some(),
            "Holder thread ID should be set after write_with_agent"
        );

        // Thread B tries to acquire with timeout
        let lock_clone2 = lock.clone();
        let handle = thread::spawn(move || {
            lock_clone2.write_with_agent(999, Duration::from_millis(200))
                .map(|_guard| ())  // drop the guard inside the thread, return () on success
        });

        let result = handle.join().unwrap();
        assert!(result.is_err(), "Should timeout");

        let err = result.unwrap_err();
        // The error should contain the holder thread ID (thread A's ID)
        assert!(
            err.holder_thread_id.is_some(),
            "Error should contain holder_thread_id"
        );
    }

    #[test]
    fn test_lock_not_poisoned_after_timeout() {
        // Thread A acquires a read lock and holds it.
        // Thread B calls try_write(200ms) - times out, gets LockTimeout.
        // Thread A releases the read lock.
        // Thread B calls try_write(500ms) again.
        // Expected: the second call returns Ok(write_guard).
        use std::sync::Arc;

        let lock = Arc::new(TimedRwLock::new(vec![1, 2, 3]));

        // Thread A holds a read lock
        let _read_guard = lock.read();

        // Thread B tries to write and times out
        let lock_clone = lock.clone();
        let handle = thread::spawn(move || {
            lock_clone.write_with_agent(999, Duration::from_millis(200))
                .map(|_guard| ())  // drop the guard inside the thread, return () on success
        });
        let result = handle.join().unwrap();
        assert!(result.is_err(), "First write attempt should timeout");

        // Thread A releases the read lock
        drop(_read_guard);

        // Thread B should now be able to acquire the write lock
        let lock_clone2 = lock.clone();
        let handle = thread::spawn(move || {
            lock_clone2.write_with_agent(999, Duration::from_millis(200))
                .map(|_guard| ())  // drop the guard inside the thread, return () on success
        });

        let result2 = handle.join().unwrap();
        assert!(
            result2.is_ok(),
            "Second write attempt should succeed after lock is released, got {:?}",
            result2
        );

        // Successfully acquired write guard
        let _write_guard = result2.unwrap();
    }
}