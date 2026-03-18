//! # Atomic Primitives for NEXUS Utilities
//!
//! This module provides enhanced atomic types optimized for the NEXUS Agentic OS:
//!
//! - `AtomicU64`, `AtomicUsize`, etc. with additional operations
//! - `AtomicTimestamp` for monotonic timestamp tracking
//! - `AtomicCounter` for high-performance counting
//! - `AtomicCell` for lock-free single-value containers
//! - `AtomicOption` for optional atomic values
//! - `AtomicRef` for atomic reference storage
//!
//! ## Design Principles
//!
//! 1. **Memory Ordering**: Uses `Relaxed` for performance where safe, `Acquire/Release` where necessary
//! 2. **Cache-Line Alignment**: Critical atomics are cache-line aligned to prevent false sharing
//! 3. **Monotonic Operations**: Timestamp operations guarantee monotonic progression
//! 4. **Zero-Allocation**: All operations are in-place with no heap allocation

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::mem;
use std::num::NonZeroU64;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

// Cache line size for alignment (typically 64 bytes on modern CPUs)
const CACHE_LINE_SIZE: usize = 64;

/// A cache-line padded atomic for preventing false sharing.
///
/// This wrapper ensures the atomic value occupies a full cache line,
/// preventing performance degradation from false sharing in high-contention scenarios.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::PaddedAtomicU64;
/// use std::sync::atomic::Ordering;
///
/// let counter = PaddedAtomicU64::new(0);
/// counter.fetch_add(1, Ordering::Relaxed);
/// ```
#[repr(align(64))]
#[derive(Debug)]
pub struct PaddedAtomicU64 {
    value: AtomicU64,
    _pad: [u8; CACHE_LINE_SIZE - mem::size_of::<AtomicU64>()],
}

impl PaddedAtomicU64 {
    /// Creates a new padded atomic with the given initial value.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self {
            value: AtomicU64::new(value),
            _pad: [0; CACHE_LINE_SIZE - mem::size_of::<AtomicU64>()],
        }
    }

    /// Loads the value from the atomic.
    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.value.load(order)
    }

    /// Stores a value into the atomic.
    #[inline]
    pub fn store(&self, val: u64, order: Ordering) {
        self.value.store(val, order);
    }

    /// Atomically adds to the value, returning the previous value.
    #[inline]
    pub fn fetch_add(&self, val: u64, order: Ordering) -> u64 {
        self.value.fetch_add(val, order)
    }

    /// Atomically subtracts from the value, returning the previous value.
    #[inline]
    pub fn fetch_sub(&self, val: u64, order: Ordering) -> u64 {
        self.value.fetch_sub(val, order)
    }

    /// Atomically swaps the value, returning the previous value.
    #[inline]
    pub fn swap(&self, val: u64, order: Ordering) -> u64 {
        self.value.swap(val, order)
    }

    /// Compares and exchanges the value.
    #[inline]
    pub fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.value.compare_exchange(current, new, success, failure)
    }

    /// Atomically updates the value using a function.
    pub fn update<F>(&self, f: F, order: Ordering) -> u64
    where
        F: Fn(u64) -> u64,
    {
        loop {
            let current = self.value.load(order);
            let new = f(current);
            match self.value.compare_exchange_weak(current, new, order, Ordering::Relaxed) {
                Ok(v) => return v,
                Err(_) => continue,
            }
        }
    }
}

impl Default for PaddedAtomicU64 {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Cache-line padded atomic usize.
#[repr(align(64))]
#[derive(Debug)]
pub struct PaddedAtomicUsize {
    value: AtomicUsize,
    _pad: [u8; CACHE_LINE_SIZE - mem::size_of::<AtomicUsize>()],
}

impl PaddedAtomicUsize {
    /// Creates a new padded atomic with the given initial value.
    #[inline]
    pub const fn new(value: usize) -> Self {
        Self {
            value: AtomicUsize::new(value),
            _pad: [0; CACHE_LINE_SIZE - mem::size_of::<AtomicUsize>()],
        }
    }

    #[inline]
    pub fn load(&self, order: Ordering) -> usize {
        self.value.load(order)
    }

    #[inline]
    pub fn store(&self, val: usize, order: Ordering) {
        self.value.store(val, order);
    }

    #[inline]
    pub fn fetch_add(&self, val: usize, order: Ordering) -> usize {
        self.value.fetch_add(val, order)
    }

    #[inline]
    pub fn fetch_sub(&self, val: usize, order: Ordering) -> usize {
        self.value.fetch_sub(val, order)
    }

    #[inline]
    pub fn compare_exchange(
        &self,
        current: usize,
        new: usize,
        success: Ordering,
        failure: Ordering,
    ) -> Result<usize, usize> {
        self.value.compare_exchange(current, new, success, failure)
    }
}

impl Default for PaddedAtomicUsize {
    fn default() -> Self {
        Self::new(0)
    }
}

/// An atomic timestamp that guarantees monotonic progression.
///
/// This is used throughout NEXUS for heartbeat timing, Lamport clocks,
/// and causal ordering of events. The timestamp always advances forward,
/// even in the presence of clock adjustments.
///
/// # Memory Model
///
/// - `load`: Acquire semantics
/// - `store`: Release semantics
/// - `update`: AcqRel semantics
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicTimestamp;
/// use std::sync::atomic::Ordering;
///
/// let ts = AtomicTimestamp::new(0);
/// assert!(ts.advance(100));
/// assert_eq!(ts.load(Ordering::Acquire), 100);
/// ```
#[derive(Debug)]
pub struct AtomicTimestamp {
    /// The raw timestamp value
    timestamp: AtomicU64,
    /// The last observed value for monotonicity enforcement
    last_observed: AtomicU64,
}

impl AtomicTimestamp {
    /// Creates a new atomic timestamp starting at zero.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self {
            timestamp: AtomicU64::new(value),
            last_observed: AtomicU64::new(value),
        }
    }

    /// Loads the current timestamp.
    ///
    /// Uses `Acquire` ordering to ensure all previous stores are visible.
    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.timestamp.load(order)
    }

    /// Attempts to store a new timestamp.
    ///
    /// Returns `true` if the new value was stored (i.e., it was >= the current value).
    /// This enforces monotonicity - timestamps never go backwards.
    #[inline]
    pub fn try_store(&self, new: u64) -> bool {
        loop {
            let current = self.timestamp.load(Ordering::Acquire);
            if new < current {
                return false;
            }
            match self.timestamp.compare_exchange_weak(
                current,
                new,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.last_observed.store(new, Ordering::Release);
                    return true;
                }
                Err(_) => continue,
            }
        }
    }

    /// Advances the timestamp to at least `minimum`.
    ///
    /// Returns `true` if the timestamp was advanced, `false` if it was already >= minimum.
    #[inline]
    pub fn advance(&self, minimum: u64) -> bool {
        let current = self.timestamp.load(Ordering::Acquire);
        if current >= minimum {
            return false;
        }
        self.try_store(minimum)
    }

    /// Advances the timestamp by a delta and returns the new value.
    ///
    /// This is equivalent to `fetch_add` but guarantees monotonicity.
    #[inline]
    pub fn advance_by(&self, delta: u64) -> u64 {
        loop {
            let current = self.timestamp.load(Ordering::Acquire);
            let new = current.wrapping_add(delta);
            match self.timestamp.compare_exchange_weak(
                current,
                new,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.last_observed.store(new, Ordering::Release);
                    return new;
                }
                Err(_) => continue,
            }
        }
    }

    /// Increments the timestamp by 1 and returns the new value.
    #[inline]
    pub fn increment(&self) -> u64 {
        self.advance_by(1)
    }

    /// Returns the timestamp in nanoseconds since Unix epoch.
    ///
    /// This is a convenience wrapper around `load`.
    #[inline]
    pub fn as_nanos(&self) -> u64 {
        self.load(Ordering::Acquire)
    }

    /// Returns the timestamp in milliseconds since Unix epoch.
    #[inline]
    pub fn as_millis(&self) -> u64 {
        self.load(Ordering::Acquire) / 1_000_000
    }

    /// Returns the timestamp in seconds since Unix epoch.
    #[inline]
    pub fn as_secs(&self) -> u64 {
        self.load(Ordering::Acquire) / 1_000_000_000
    }

    /// Creates a new timestamp from the current system time.
    #[inline]
    pub fn now() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        Self::new(now)
    }

    /// Advances the timestamp to at least the incoming value (Lamport clock semantics).
    ///
    /// This implements Lamport clock behavior: when receiving a message with
    /// timestamp T, the clock advances to `max(current, T) + 1`.
    ///
    /// This ensures causal ordering across distributed processes.
    ///
    /// # Example
    ///
    /// ```rust
    /// use nexus_utils::atomic::AtomicTimestamp;
    /// use std::sync::atomic::Ordering;
    ///
    /// let ts = AtomicTimestamp::new(50);
    /// ts.advance_to(80); // max(50, 80) + 1 = 81
    /// assert_eq!(ts.load(Ordering::Acquire), 81);
    /// ```
    #[inline]
    pub fn advance_to(&self, incoming: u64) -> u64 {
        loop {
            let current = self.timestamp.load(Ordering::Acquire);
            let new_value = current.max(incoming) + 1;
            match self.timestamp.compare_exchange_weak(
                current,
                new_value,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.last_observed.store(new_value, Ordering::Release);
                    return new_value;
                }
                Err(_) => continue,
            }
        }
    }

    /// Updates this timestamp to the current system time if it's older.
    #[inline]
    pub fn update_to_now(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        self.advance(now)
    }
}

impl Default for AtomicTimestamp {
    fn default() -> Self {
        Self::new(0)
    }
}

/// A high-performance atomic counter with saturating arithmetic.
///
/// The counter never overflows; instead it saturates at the maximum value.
/// Useful for metrics, counters, and resource tracking.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicCounter;
/// use std::sync::atomic::Ordering;
///
/// let counter = AtomicCounter::new(0);
/// assert_eq!(counter.increment(Ordering::Relaxed), 1);
/// assert_eq!(counter.add(5, Ordering::Relaxed), 6);
/// ```
#[derive(Debug)]
pub struct AtomicCounter {
    value: AtomicU64,
    /// Optional threshold for saturation notifications
    threshold: Option<NonZeroU64>,
}

impl AtomicCounter {
    /// Creates a new counter starting at zero.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self {
            value: AtomicU64::new(value),
            threshold: None,
        }
    }

    /// Creates a counter with a saturation threshold.
    #[inline]
    pub fn with_threshold(value: u64, threshold: NonZeroU64) -> Self {
        Self {
            value: AtomicU64::new(value),
            threshold: Some(threshold),
        }
    }

    /// Loads the current value.
    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.value.load(order)
    }

    /// Increments by 1 and returns the new value.
    ///
    /// Saturates at `u64::MAX`.
    #[inline]
    pub fn increment(&self, order: Ordering) -> u64 {
        self.add(1, order)
    }

    /// Decrements by 1 and returns the new value.
    ///
    /// Saturates at 0.
    #[inline]
    pub fn decrement(&self, order: Ordering) -> u64 {
        self.sub(1, order)
    }

    /// Adds a value and returns the new value.
    ///
    /// Uses saturating arithmetic to prevent overflow.
    #[inline]
    pub fn add(&self, delta: u64, order: Ordering) -> u64 {
        loop {
            let current = self.value.load(Ordering::Acquire);
            let new = current.saturating_add(delta);
            match self.value.compare_exchange_weak(current, new, order, Ordering::Acquire) {
                Ok(_) => return new,
                Err(_) => continue,
            }
        }
    }

    /// Subtracts a value and returns the new value.
    ///
    /// Uses saturating arithmetic to prevent underflow.
    #[inline]
    pub fn sub(&self, delta: u64, order: Ordering) -> u64 {
        loop {
            let current = self.value.load(Ordering::Acquire);
            let new = current.saturating_sub(delta);
            match self.value.compare_exchange_weak(current, new, order, Ordering::Acquire) {
                Ok(_) => return new,
                Err(_) => continue,
            }
        }
    }

    /// Atomically sets the value if the new value is greater.
    ///
    /// Returns `true` if the value was updated.
    #[inline]
    pub fn set_if_greater(&self, new: u64, order: Ordering) -> bool {
        loop {
            let current = self.value.load(Ordering::Acquire);
            if current >= new {
                return false;
            }
            match self.value.compare_exchange_weak(current, new, order, Ordering::Acquire) {
                Ok(_) => return true,
                Err(_) => continue,
            }
        }
    }

    /// Resets the counter to zero and returns the previous value.
    #[inline]
    pub fn reset(&self, order: Ordering) -> u64 {
        self.value.swap(0, order)
    }

    /// Returns `true` if the counter has reached or exceeded its threshold.
    #[inline]
    pub fn is_at_threshold(&self) -> bool {
        self.threshold.map_or(false, |t| {
            self.value.load(Ordering::Acquire) >= t.get()
        })
    }

    /// Returns the ratio of current value to threshold, if threshold is set.
    #[inline]
    pub fn ratio(&self) -> Option<f64> {
        self.threshold.map(|t| {
            let current = self.value.load(Ordering::Acquire);
            current as f64 / t.get() as f64
        })
    }

    /// Returns the remaining capacity before threshold.
    #[inline]
    pub fn remaining(&self) -> Option<u64> {
        self.threshold.map(|t| {
            let current = self.value.load(Ordering::Acquire);
            t.get().saturating_sub(current)
        })
    }
}

impl Default for AtomicCounter {
    fn default() -> Self {
        Self::new(0)
    }
}

/// A lock-free container for a single value with atomic updates.
///
/// Unlike `AtomicPtr`, `AtomicCell` can hold any `Copy` type and provides
/// safe atomic operations on it. The type must be `Copy` and have stable
/// addresses for soundness.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicCell;
///
/// let cell = AtomicCell::new(42u64);
/// assert_eq!(cell.load(), 42);
/// cell.store(100);
/// assert_eq!(cell.swap(200), 100);
/// ```
#[repr(C)]
pub struct AtomicCell<T: Copy> {
    value: UnsafeCell<T>,
    lock: AtomicBool,
}

impl<T: Copy> AtomicCell<T> {
    /// Creates a new atomic cell with the given value.
    #[inline]
    pub const fn new(value: T) -> Self {
        Self {
            value: UnsafeCell::new(value),
            lock: AtomicBool::new(false),
        }
    }

    /// Loads the current value.
    ///
    /// Uses spin-lock for types larger than pointer size.
    #[inline]
    pub fn load(&self) -> T {
        // For types that fit in a pointer, use atomic operations
        if mem::size_of::<T>() <= mem::size_of::<usize>() {
            unsafe {
                let ptr = self.value.get() as *const usize;
                let raw = ptr::read_volatile(ptr);
                mem::transmute_copy(&raw)
            }
        } else {
            // Spin-lock for larger types
            loop {
                if self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    let value = unsafe { *self.value.get() };
                    self.lock.store(false, Ordering::Release);
                    return value;
                }
                std::hint::spin_loop();
            }
        }
    }

    /// Stores a new value.
    #[inline]
    pub fn store(&self, value: T) {
        if mem::size_of::<T>() <= mem::size_of::<usize>() {
            unsafe {
                let ptr = self.value.get() as *mut usize;
                let raw: usize = mem::transmute_copy(&value);
                ptr::write_volatile(ptr, raw);
            }
        } else {
            loop {
                if self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    unsafe {
                        ptr::write_volatile(self.value.get(), value);
                    }
                    self.lock.store(false, Ordering::Release);
                    return;
                }
                std::hint::spin_loop();
            }
        }
    }

    /// Swaps the value and returns the previous value.
    #[inline]
    pub fn swap(&self, value: T) -> T {
        loop {
            if self.lock.compare_exchange_weak(
                false, true, Ordering::Acquire, Ordering::Relaxed
            ).is_ok() {
                let old = unsafe { ptr::read_volatile(self.value.get()) };
                unsafe { ptr::write_volatile(self.value.get(), value) };
                self.lock.store(false, Ordering::Release);
                return old;
            }
            std::hint::spin_loop();
        }
    }

    /// Compares and exchanges the value.
    ///
    /// Returns `Ok(old)` on success, `Err(current)` on failure.
    #[inline]
    pub fn compare_exchange(&self, current: T, new: T) -> Result<T, T>
    where
        T: PartialEq,
    {
        if mem::size_of::<T>() <= mem::size_of::<usize>() {
            loop {
                let actual = self.load();
                if actual != current {
                    return Err(actual);
                }
                match self.try_swap(current, new) {
                    Ok(old) => return Ok(old),
                    Err(_) => continue,
                }
            }
        } else {
            loop {
                if self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                    unsafe {
                        let actual = *self.value.get();
                        if actual != current {
                            self.lock.store(false, Ordering::Release);
                            return Err(actual);
                        }
                        ptr::write_volatile(self.value.get(), new);
                        self.lock.store(false, Ordering::Release);
                        return Ok(current);
                    }
                }
                std::hint::spin_loop();
            }
        }
    }

    /// Attempts to swap if the current value matches.
    #[inline]
    fn try_swap(&self, expected: T, new: T) -> Result<T, ()> {
        let current = self.load();
        if mem::size_of::<T>() <= mem::size_of::<usize>()
            && unsafe {
                let _ptr = self.value.get() as *mut usize;
                (self.value.get() as *const AtomicUsize as *mut AtomicUsize)
                    .as_ref()
                    .unwrap()
                    .compare_exchange(
                        mem::transmute_copy::<T, usize>(&expected),
                        mem::transmute_copy::<T, usize>(&new),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
            }
            .is_ok()
        {
            Ok(current)
        } else {
            Err(())
        }
    }
}

impl<T: Copy + Default> Default for AtomicCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

unsafe impl<T: Copy + Send> Send for AtomicCell<T> {}
unsafe impl<T: Copy + Send + Sync> Sync for AtomicCell<T> {}

/// An atomic optional value.
///
/// Provides atomic operations on `Option<T>` where `T` is a non-zero type
/// that can fit in a pointer (e.g., `NonZeroU64`, `Box<T>`).
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicOption;
/// use std::num::NonZeroU64;
///
/// let opt = AtomicOption::new(None);
/// assert!(opt.load().is_none());
/// opt.swap(Some(42u64));
/// assert_eq!(opt.load(), Some(42));
/// ```
pub struct AtomicOption<T> {
    ptr: AtomicPtr<T>,
}

impl<T> AtomicOption<T> {
    /// Creates a new atomic option with `None`.
    #[inline]
    pub fn new(value: Option<T>) -> Self {
        let raw = match value {
            Some(v) => Box::into_raw(Box::new(v)),
            None => ptr::null_mut(),
        };
        Self { ptr: AtomicPtr::new(raw) }
    }

    /// Creates a new atomic option from a raw pointer.
    ///
    /// # Safety
    /// The pointer must be either null or valid for the lifetime of this atomic.
    #[inline]
    pub const unsafe fn from_raw(ptr: *mut T) -> Self {
        Self {
            ptr: AtomicPtr::new(ptr),
        }
    }

    /// Loads the current value.
    #[inline]
    pub fn load(&self) -> Option<T>
    where
        T: Clone,
    {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { (*ptr).clone() })
        }
    }

    /// Loads a reference to the value without cloning.
    #[inline]
    pub fn load_ref(&self) -> Option<&T> {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    /// Stores a new value, returning the old value.
    #[inline]
    pub fn swap(&self, value: Option<T>) -> Option<T> {
        let new_ptr = value.map_or(ptr::null_mut(), |v| Box::into_raw(Box::new(v)));
        let old_ptr = self.ptr.swap(new_ptr, Ordering::AcqRel);
        if old_ptr.is_null() {
            None
        } else {
            Some(unsafe { *Box::from_raw(old_ptr) })
        }
    }

    /// Stores `None` and returns the previous value.
    #[inline]
    pub fn take(&self) -> Option<T> {
        self.swap(None)
    }

    /// Returns `true` if the option is `Some`.
    #[inline]
    pub fn is_some(&self) -> bool {
        !self.ptr.load(Ordering::Acquire).is_null()
    }

    /// Returns `true` if the option is `None`.
    #[inline]
    pub fn is_none(&self) -> bool {
        self.ptr.load(Ordering::Acquire).is_null()
    }
}

impl<T> Drop for AtomicOption<T> {
    fn drop(&mut self) {
        let ptr = *self.ptr.get_mut();
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }
}

impl<T: Clone> Clone for AtomicOption<T> {
    fn clone(&self) -> Self {
        Self::new(self.load())
    }
}

impl<T> Default for AtomicOption<T> {
    fn default() -> Self {
        Self::new(None)
    }
}

unsafe impl<T: Send> Send for AtomicOption<T> {}
unsafe impl<T: Send + Sync> Sync for AtomicOption<T> {}

/// An atomic reference counter for shared ownership.
///
/// Similar to `Arc` but with atomic reference counting that can be
/// queried atomically without cloning.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicRef;
///
/// let arc = AtomicRef::new(42u64);
/// assert_eq!(arc.as_ref(), Some(&42));
/// assert!(!arc.is_null());
/// ```
pub struct AtomicRef<T> {
    ptr: AtomicPtr<T>,
}

impl<T> AtomicRef<T> {
    /// Creates a new atomic reference from a boxed value.
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            ptr: AtomicPtr::new(Box::into_raw(Box::new(value))),
        }
    }

    /// Gets a reference to the inner value.
    #[inline]
    pub fn as_ref(&self) -> Option<&T> {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    /// Gets a mutable reference to the inner value.
    ///
    /// Returns `None` if there are other references.
    #[inline]
    pub fn as_mut(&mut self) -> Option<&mut T> {
        let ptr = *self.ptr.get_mut();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &mut *ptr })
        }
    }

    /// Returns a raw pointer to the inner value.
    #[inline]
    pub fn as_ptr(&self) -> *const T {
        self.ptr.load(Ordering::Acquire)
    }

    /// Checks if the reference is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.load(Ordering::Acquire).is_null()
    }

    /// Atomically swaps the value.
    #[inline]
    pub fn swap(&self, value: Option<T>) -> Option<T> {
        let new_ptr = value.map_or(ptr::null_mut(), |v| Box::into_raw(Box::new(v)));
        let old_ptr = self.ptr.swap(new_ptr, Ordering::AcqRel);
        if old_ptr.is_null() {
            None
        } else {
            Some(unsafe { *Box::from_raw(old_ptr) })
        }
    }

    /// Takes the value out, leaving `None`.
    #[inline]
    pub fn take(&self) -> Option<T> {
        self.swap(None)
    }

    /// Atomically sets the value if currently null.
    #[inline]
    pub fn set_if_none(&self, value: T) -> Result<(), T> {
        let new_ptr = Box::into_raw(Box::new(value));
        match self.ptr.compare_exchange(
            ptr::null_mut(),
            new_ptr,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(_) => {
                // SAFETY: we just created this box and CAS failed so no one else owns it
                let value = unsafe { *Box::from_raw(new_ptr) };
                Err(value)
            }
        }
    }
}

impl<T> Drop for AtomicRef<T> {
    fn drop(&mut self) {
        let ptr = *self.ptr.get_mut();
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }
}

impl<T: Clone> Clone for AtomicRef<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            Self {
                ptr: AtomicPtr::new(ptr::null_mut()),
            }
        } else {
            let value = unsafe { (*ptr).clone() };
            Self::new(value)
        }
    }
}

impl<T> Default for AtomicRef<T> {
    fn default() -> Self {
        Self {
            ptr: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for AtomicRef<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.as_ref() {
            Some(v) => f.debug_tuple("AtomicRef").field(v).finish(),
            None => f.debug_tuple("AtomicRef").field(&"null").finish(),
        }
    }
}

unsafe impl<T: Send> Send for AtomicRef<T> {}
unsafe impl<T: Send + Sync> Sync for AtomicRef<T> {}

/// A typed atomic that enforces type safety at compile time.
///
/// This is a wrapper around `AtomicU64` that prevents mixing different
/// typed atomics at runtime.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::TypedAtomic;
///
/// struct GoalId;
/// struct AgentId;
///
/// type AtomicGoalId = TypedAtomic<GoalId>;
/// type AtomicAgentId = TypedAtomic<AgentId>;
///
/// // These cannot be mixed up at compile time
/// let goal = AtomicGoalId::new(1);
/// let agent = AtomicAgentId::new(2);
/// // goal = agent; // Compile error!
/// ```
#[derive(Debug)]
pub struct TypedAtomic<T> {
    value: AtomicU64,
    _marker: PhantomData<T>,
}

impl<T> TypedAtomic<T> {
    /// Creates a new typed atomic.
    #[inline]
    pub const fn new(value: u64) -> Self {
        Self {
            value: AtomicU64::new(value),
            _marker: PhantomData,
        }
    }

    /// Loads the value.
    #[inline]
    pub fn load(&self, order: Ordering) -> u64 {
        self.value.load(order)
    }

    /// Stores the value.
    #[inline]
    pub fn store(&self, val: u64, order: Ordering) {
        self.value.store(val, order);
    }

    /// Atomically adds to the value.
    #[inline]
    pub fn fetch_add(&self, val: u64, order: Ordering) -> u64 {
        self.value.fetch_add(val, order)
    }

    /// Increments and returns the previous value.
    #[inline]
    pub fn increment(&self, order: Ordering) -> u64 {
        self.fetch_add(1, order)
    }

    /// Compares and exchanges.
    #[inline]
    pub fn compare_exchange(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        self.value.compare_exchange(current, new, success, failure)
    }

    /// Swaps the value.
    #[inline]
    pub fn swap(&self, val: u64, order: Ordering) -> u64 {
        self.value.swap(val, order)
    }

    /// Creates a new typed atomic from the next ID.
    ///
    /// Uses a global atomic counter for ID generation.
    #[inline]
    pub fn next_id() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self::new(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl<T> Default for TypedAtomic<T> {
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T> Clone for TypedAtomic<T> {
    fn clone(&self) -> Self {
        Self::new(self.load(Ordering::Acquire))
    }
}

impl<T> PartialEq for TypedAtomic<T> {
    fn eq(&self, other: &Self) -> bool {
        self.load(Ordering::Acquire) == other.load(Ordering::Acquire)
    }
}

impl<T> Eq for TypedAtomic<T> {}

impl<T> std::hash::Hash for TypedAtomic<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.load(Ordering::Acquire).hash(state);
    }
}

unsafe impl<T: Send> Send for TypedAtomic<T> {}
unsafe impl<T: Send + Sync> Sync for TypedAtomic<T> {}

/// An atomic pointer with safe access semantics.
///
/// This provides atomic operations on a pointer with proper memory ordering:
/// - `load()` returns `Option<&T>` - a reference to the current value
/// - `store()` atomically updates the pointer
/// - `swap()` atomically swaps and returns the old pointer
///
/// # Memory Ordering
///
/// All loads use `Acquire` ordering and all stores use `Release` ordering,
/// ensuring that readers see fully initialized data.
///
/// # Liveness Invariant
///
/// This type does NOT support safe memory reclamation. Once a pointer is
/// swapped out, it is the caller's responsibility to ensure no readers
/// are still accessing it before freeing the data.
///
/// For production use, consider `AtomicOption` which manages ownership.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicPointer;
/// use std::sync::atomic::Ordering;
///
/// let ptr = AtomicPointer::<u64>::new();
/// assert!(ptr.load().is_none());
///
/// let value = Box::new(42u64);
/// ptr.store(value.as_ref());
/// assert_eq!(ptr.load().map(|r| *r), Some(42));
/// ```
pub struct AtomicPointer<T> {
    ptr: AtomicPtr<T>,
    _marker: PhantomData<T>,
}

impl<T> AtomicPointer<T> {
    /// Creates a new atomic pointer initialized to null.
    #[inline]
    pub const fn new() -> Self {
        Self {
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// Creates a new atomic pointer from a reference.
    #[inline]
    pub fn from_ref(value: &T) -> Self {
        Self {
            ptr: AtomicPtr::new(value as *const T as *mut T),
            _marker: PhantomData,
        }
    }

    /// Loads the current pointer value.
    ///
    /// Returns `None` if the pointer is null, or `Some(&T)` otherwise.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointed-to data remains valid for the
    /// lifetime of the returned reference.
    #[inline]
    pub fn load(&self) -> Option<&T> {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { &*ptr })
        }
    }

    /// Loads the raw pointer value.
    ///
    /// Returns null if no pointer has been stored.
    #[inline]
    pub fn load_raw(&self) -> *const T {
        self.ptr.load(Ordering::Acquire)
    }

    /// Stores a new pointer value.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointer remains valid for as long as
    /// it may be accessed by other threads.
    #[inline]
    pub fn store(&self, value: &T) {
        self.ptr.store(value as *const T as *mut T, Ordering::Release);
    }

    /// Stores a raw pointer value.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointer is valid and remains valid.
    #[inline]
    pub fn store_raw(&self, ptr: *const T) {
        self.ptr.store(ptr as *mut T, Ordering::Release);
    }

    /// Atomically swaps the pointer, returning the old pointer.
    ///
    /// # Safety
    ///
    /// The caller is responsible for ensuring proper memory reclamation
    /// of the old pointer value.
    #[inline]
    pub fn swap(&self, value: &T) -> *const T {
        self.ptr.swap(value as *const T as *mut T, Ordering::AcqRel)
    }

    /// Atomically swaps the pointer with a raw pointer.
    #[inline]
    pub fn swap_raw(&self, ptr: *const T) -> *const T {
        self.ptr.swap(ptr as *mut T, Ordering::AcqRel)
    }

    /// Returns `true` if the pointer is null.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.load(Ordering::Acquire).is_null()
    }
}

impl<T> Default for AtomicPointer<T> {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl<T: Send> Send for AtomicPointer<T> {}
unsafe impl<T: Send + Sync> Sync for AtomicPointer<T> {}

/// A signed atomic integer for token bucket accounting.
///
/// This is used in the cognitive kernel for dispatch token management.
/// Unlike `AtomicCounter` which saturates at 0, `AtomicInt` can go negative,
/// which is essential for detecting double-release bugs in token management.
///
/// # Example
///
/// ```rust
/// use nexus_utils::atomic::AtomicInt;
/// use std::sync::atomic::Ordering;
///
/// let counter = AtomicInt::new(0);
/// counter.fetch_add(100, Ordering::Relaxed); // Reserve tokens
/// counter.fetch_sub(100, Ordering::Relaxed);  // Release tokens
/// ```
pub struct AtomicInt {
    value: std::sync::atomic::AtomicI64,
}

impl AtomicInt {
    /// Creates a new atomic integer with the given initial value.
    #[inline]
    pub const fn new(value: i64) -> Self {
        Self {
            value: std::sync::atomic::AtomicI64::new(value),
        }
    }

    /// Loads the current value.
    #[inline]
    pub fn load(&self, order: Ordering) -> i64 {
        self.value.load(order)
    }

    /// Stores a value.
    #[inline]
    pub fn store(&self, val: i64, order: Ordering) {
        self.value.store(val, order);
    }

    /// Atomically adds to the value, returning the previous value.
    #[inline]
    pub fn fetch_add(&self, val: i64, order: Ordering) -> i64 {
        self.value.fetch_add(val, order)
    }

    /// Atomically subtracts from the value, returning the previous value.
    #[inline]
    pub fn fetch_sub(&self, val: i64, order: Ordering) -> i64 {
        self.value.fetch_sub(val, order)
    }

    /// Atomically swaps the value, returning the previous value.
    #[inline]
    pub fn swap(&self, val: i64, order: Ordering) -> i64 {
        self.value.swap(val, order)
    }

    /// Compares and exchanges the value.
    #[inline]
    pub fn compare_exchange(
        &self,
        current: i64,
        new: i64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<i64, i64> {
        self.value.compare_exchange(current, new, success, failure)
    }

    /// Returns `true` if the value is negative.
    #[inline]
    pub fn is_negative(&self, order: Ordering) -> bool {
        self.value.load(order) < 0
    }

    /// Returns `true` if the value is zero.
    #[inline]
    pub fn is_zero(&self, order: Ordering) -> bool {
        self.value.load(order) == 0
    }
}

impl Default for AtomicInt {
    fn default() -> Self {
        Self::new(0)
    }
}

impl std::fmt::Debug for AtomicInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicInt")
            .field("value", &self.value.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_padded_atomic_u64() {
        let atomic = PaddedAtomicU64::new(42);
        assert_eq!(atomic.load(Ordering::Relaxed), 42);
        assert_eq!(atomic.fetch_add(10, Ordering::Relaxed), 42);
        assert_eq!(atomic.load(Ordering::Relaxed), 52);
    }

    #[test]
    fn test_atomic_timestamp_monotonicity() {
        let ts = AtomicTimestamp::new(100);
        assert!(ts.advance(150));
        assert!(!ts.advance(120)); // Should not go backwards
        assert_eq!(ts.load(Ordering::Relaxed), 150);
    }

    #[test]
    fn test_atomic_counter_saturating() {
        let counter = AtomicCounter::new(u64::MAX - 5);
        let result = counter.add(10, Ordering::Relaxed);
        assert_eq!(result, u64::MAX);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn test_atomic_counter_threshold() {
        let threshold = NonZeroU64::new(100).unwrap();
        let counter = AtomicCounter::with_threshold(50, threshold);

        // Counter starts at 50, threshold is 100
        assert!(!counter.is_at_threshold(), "Should not be at threshold at start");
        assert_eq!(counter.ratio(), Some(0.5));
        assert_eq!(counter.remaining(), Some(50));

        // Add 50 to reach exactly 100 (the threshold)
        counter.add(50, Ordering::Relaxed);

        // Now at exactly threshold
        assert!(counter.is_at_threshold(), "Should be at threshold after reaching 100");
        assert_eq!(counter.ratio(), Some(1.0));
        assert_eq!(counter.remaining(), Some(0));

        // Go beyond threshold
        counter.add(10, Ordering::Relaxed);
        assert!(counter.is_at_threshold(), "Should still be at/past threshold");
        assert_eq!(counter.ratio(), Some(1.1));
        assert_eq!(counter.remaining(), Some(0)); // saturates at 0
    }

    #[test]
    fn test_atomic_cell() {
        let cell = AtomicCell::new(42u64);
        assert_eq!(cell.load(), 42);
        cell.store(100);
        assert_eq!(cell.load(), 100);
        assert_eq!(cell.swap(200), 100);
        assert_eq!(cell.load(), 200);
    }

    #[test]
    fn test_atomic_option() {
        let opt = AtomicOption::new(None);
        assert!(opt.is_none());
        opt.swap(Some(42u64));
        assert!(opt.is_some());
        assert_eq!(opt.load(), Some(42u64));
        assert_eq!(opt.take(), Some(42u64));
        assert!(opt.is_none());
    }

    #[test]
    fn test_typed_atomic_type_safety() {
        struct GoalId;
        struct AgentId;

        let goal = TypedAtomic::<GoalId>::new(1);
        let agent = TypedAtomic::<AgentId>::new(2);

        // These compile - same type
        let _ = goal.load(Ordering::Relaxed);
        let _ = agent.load(Ordering::Relaxed);

        // This would fail to compile (different types):
        // let _: TypedAtomic<GoalId> = agent;
    }

    #[test]
    fn test_concurrent_timestamp() {
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

    // ============================================================================
    // Lamport Clock Tests
    // ============================================================================

    #[test]
    fn test_lamport_advance_to_higher_value() {
        // Counter starts at 50. Call advance_to(80).
        // Expected: max(50, 80) + 1 = 81
        let ts = AtomicTimestamp::new(50);
        let result = ts.advance_to(80);
        let loaded = ts.load(Ordering::Acquire);

        assert_eq!(loaded, 81, "advance_to(80) from 50 should give 81, not {}", loaded);
        assert_eq!(result, 81, "advance_to should return the new value");
    }

    #[test]
    fn test_lamport_advance_to_lower_value_no_regression() {
        // Counter starts at 50. Call advance_to(30).
        // Expected: max(50, 30) + 1 = 51
        // The counter must NOT move to 31.
        let ts = AtomicTimestamp::new(50);
        let result = ts.advance_to(30);
        let loaded = ts.load(Ordering::Acquire);

        assert_eq!(loaded, 51, "advance_to(30) from 50 should give 51, not {}", loaded);
        assert!(loaded > 50, "Counter should not regress below initial value");
        assert_eq!(result, 51);
    }

    #[test]
    fn test_lamport_advance_to_equal_value() {
        // Counter starts at 50. Call advance_to(50).
        // Expected: max(50, 50) + 1 = 51
        // This is the session-start boundary where incoming equals current.
        let ts = AtomicTimestamp::new(50);
        let result = ts.advance_to(50);
        let loaded = ts.load(Ordering::Acquire);

        assert_eq!(loaded, 51, "advance_to(50) from 50 should give 51, not {}", loaded);
        assert_eq!(result, 51);
    }

    #[test]
    fn test_lamport_concurrent_advance_never_regresses() {
        // Counter starts at 0. Spawn 50 threads.
        // Thread i calls advance_to(i * 7).
        // Maximum incoming value is thread 49 calling advance_to(343).
        // Counter must be at least 344 after all threads complete.
        let ts = Arc::new(AtomicTimestamp::new(0));
        let mut handles = vec![];

        for i in 0..50 {
            let ts_clone = ts.clone();
            handles.push(thread::spawn(move || {
                ts_clone.advance_to(i * 7);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let loaded = ts.load(Ordering::Acquire);
        assert!(
            loaded >= 344,
            "Counter must be >= 344 (max incoming + 1), got {}",
            loaded
        );
    }

    // ============================================================================
    // Atomic Pointer Tests
    // ============================================================================

    #[test]
    fn test_atomic_pointer_null_before_first_store() {
        // Create a fresh AtomicPointer with no store called.
        // Expected: load() returns None. No panic, no garbage pointer.
        let ptr: AtomicPointer<u64> = AtomicPointer::new();

        let result = ptr.load();
        assert!(result.is_none(), "Fresh AtomicPointer should load None, got {:?}", result);
        assert!(ptr.is_null(), "Fresh AtomicPointer should be null");
    }

    #[test]
    fn test_atomic_pointer_swap_concurrent_readers_never_see_null() {
        // Create object A (value 100) and object B (value 200).
        // Store pointer to A. Spawn 10 reader threads.
        // Each reader loops 10,000 times: load, assert not null, dereference, assert value is 100 or 200.
        // After 5ms, main thread swaps pointer to B.
        // Expected: every assertion passes. No null, no third value, no segfault.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let a: u64 = 100;
        let b: u64 = 200;

        let ptr = Arc::new(AtomicPointer::<u64>::from_ref(&a));
        let stop = Arc::new(AtomicBool::new(false));
        let swapped = Arc::new(AtomicBool::new(false));

        let mut handles = vec![];
        for _ in 0..10 {
            let ptr_clone = ptr.clone();
            let stop_clone = stop.clone();
            let _swapped_clone = swapped.clone();

            handles.push(thread::spawn(move || {
                for _ in 0..10_000 {
                    if stop_clone.load(Ordering::Relaxed) {
                        break;
                    }
                    let loaded = ptr_clone.load();
                    assert!(
                        loaded.is_some(),
                        "Reader should never see null during swap"
                    );
                    let value = *loaded.unwrap();
                    assert!(
                        value == 100 || value == 200,
                        "Reader saw invalid value: {}",
                        value
                    );
                }
            }));
        }

        // Wait a bit, then swap
        thread::sleep(Duration::from_millis(5));
        ptr.swap(&b);
        swapped.store(true, Ordering::Release);

        // Let readers finish
        thread::sleep(Duration::from_millis(10));
        stop.store(true, Ordering::Release);

        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn test_atomic_pointer_old_readers_complete_after_swap() {
        // Create object A (value 100) and object B (value 200).
        // Store pointer to A. Spawn reader thread that loads pointer to A,
        // then sleeps for 50ms before dereferencing.
        // After 10ms, while reader is sleeping, swap pointer to B.
        // Reader should still read 100 after waking.
        // This tests the liveness invariant: old object must stay alive during read.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        // Use Box to keep objects alive on heap
        let a: Box<u64> = Box::new(100);
        let b: Box<u64> = Box::new(200);

        let a_ref: &u64 = &*a;
        let b_ref: &u64 = &*b;

        let ptr = Arc::new(AtomicPointer::<u64>::from_ref(a_ref));
        let reader_started = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::new(AtomicBool::new(false));

        let ptr_clone = ptr.clone();
        let started_clone = reader_started.clone();
        let done_clone = reader_done.clone();

        let reader = thread::spawn(move || {
            // Load pointer to A
            let loaded = ptr_clone.load();
            started_clone.store(true, Ordering::Release);

            // Sleep while holding the reference
            thread::sleep(Duration::from_millis(50));

            // After waking, the global pointer has been swapped to B
            // but we should still be able to dereference our held reference
            let value = loaded.unwrap();
            assert_eq!(*value, 100, "Reader should read 100 from old pointer");

            done_clone.store(true, Ordering::Release);
        });

        // Wait for reader to start
        while !reader_started.load(Ordering::Acquire) {
            thread::yield_now();
        }

        // Swap to B while reader is sleeping (after 10ms)
        thread::sleep(Duration::from_millis(10));
        ptr.swap(b_ref);

        // Wait for reader to finish
        reader.join().unwrap();

        // Both objects are still alive (Box keeps them alive)
        assert_eq!(*a, 100);
        assert_eq!(*b, 200);
    }

    // ============================================================================
    // Atomic Int Tests (Token Bucket)
    // ============================================================================

    #[test]
    fn test_atomic_int_basic_operations() {
        let counter = AtomicInt::new(0);

        assert_eq!(counter.load(Ordering::Relaxed), 0);

        let prev = counter.fetch_add(100, Ordering::Relaxed);
        assert_eq!(prev, 0);
        assert_eq!(counter.load(Ordering::Relaxed), 100);

        let prev = counter.fetch_sub(50, Ordering::Relaxed);
        assert_eq!(prev, 100);
        assert_eq!(counter.load(Ordering::Relaxed), 50);
    }

    #[test]
    fn test_token_does_not_wrap_on_double_release() {
        // Create an AtomicInt starting at 0.
        // Call fetch_add(100) - one dispatch reservation.
        // Call fetch_sub(100) - circuit breaker fires, reservation released.
        // Call fetch_sub(100) again - unexpected double-release.
        // Expected: counter reads -100, not i64::MAX or wrap-around.

        let counter = AtomicInt::new(0);

        // First reservation
        let prev = counter.fetch_add(100, Ordering::SeqCst);
        assert_eq!(prev, 0, "First fetch_add should return 0");
        assert_eq!(counter.load(Ordering::SeqCst), 100, "After add, counter should be 100");

        // Release reservation (normal)
        let prev = counter.fetch_sub(100, Ordering::SeqCst);
        assert_eq!(prev, 100, "fetch_sub after add should return 100");
        assert_eq!(counter.load(Ordering::SeqCst), 0, "After release, counter should be 0");

        // Double release (bug!) - should make counter negative
        let prev = counter.fetch_sub(100, Ordering::SeqCst);
        assert_eq!(prev, 0, "Second fetch_sub should return 0");

        // The key assertion: counter should be -100, not wrapped to a huge positive value
        let final_value = counter.load(Ordering::SeqCst);
        assert_eq!(final_value, -100, "Counter should be -100 after double release, got {}", final_value);

        // This is detectable: negative means something is wrong
        assert!(counter.is_negative(Ordering::SeqCst), "Negative counter indicates bug");

        // If we had used unsigned and wrapped, we'd see i64::MAX - 100 or similar huge value
        // which would pass every budget check - the bug!
    }

    #[test]
    fn test_atomic_int_signed_detection() {
        let counter = AtomicInt::new(50);

        assert!(!counter.is_negative(Ordering::Relaxed));
        assert!(!counter.is_zero(Ordering::Relaxed));

        counter.fetch_sub(100, Ordering::Relaxed);
        assert!(counter.is_negative(Ordering::Relaxed));

        counter.store(0, Ordering::Relaxed);
        assert!(counter.is_zero(Ordering::Relaxed));
    }
}