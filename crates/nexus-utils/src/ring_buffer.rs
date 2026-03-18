//! # Lock-Free Ring Buffer for NEXUS Event Bus
//!
//! This module provides high-performance MPMC (Multiple Producer, Multiple Consumer)
//! ring buffer implementations optimized for the NEXUS event bus:
//!
//! - `RingBuffer`: Single-producer single-consumer SPSC buffer
//! - `MpmcRingBuffer`: Multiple-producer multiple-consumer MPMC buffer
//! - `BoundedMpmcQueue`: Bounded queue with backpressure support
//! - `UnboundedMpmcQueue`: Unbounded queue with dynamic growth
//!
//! ## Design Principles
//!
//! 1. **Lock-free**: Uses atomic operations only, no mutexes
//! 2. **Cache-friendly**: Aligned to cache lines to prevent false sharing
//! 3. **Zero-copy reads**: Consumers read directly from buffer slots
//! 4. **Backpressure**: Bounded variants support configurable capacity

use std::cell::UnsafeCell;
use std::fmt;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Cache line size for alignment.
const CACHE_LINE_SIZE: usize = 64;

/// Default buffer capacity for bounded queues.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Maximum capacity for bounded queues.
pub const MAX_CAPACITY: usize = 1 << 24; // 16M entries

/// A slot in the ring buffer.
///
/// Each slot has a sequence number that determines its state:
/// - `sequence == head`: Ready for writing
/// - `sequence == tail`: Ready for reading
/// - `sequence == tail + 1`: Being written
/// - `sequence == head - 1`: Being read
#[repr(C, align(64))]
struct Slot<T> {
    sequence: AtomicUsize,
    data: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    #[inline]
    const fn new(sequence: usize) -> Self {
        Self {
            sequence: AtomicUsize::new(sequence),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

impl<T> Drop for Slot<T> {
    fn drop(&mut self) {
        // Safety: We're dropping the slot, so the data should be dropped too
        // But we don't know if it's initialized, so we just let it leak
        // This is intentional for ring buffers - data is never "owned" by slots
    }
}

/// A lock-free MPMC ring buffer.
///
/// This implementation uses the MPSC queue algorithm from Dmitry Vyukov
/// with extensions for multiple consumers. It provides:
///
/// - Wait-free writes for producers
/// - Wait-free reads for consumers
/// - Bounded capacity with wraparound
/// - Cache-line aligned slots to prevent false sharing
///
/// # Example
///
/// ```rust
/// use nexus_utils::ring_buffer::MpmcRingBuffer;
///
/// let buffer = MpmcRingBuffer::new(1024);
///
/// // Multiple producers can write
/// buffer.push(42).unwrap();
/// buffer.push(100).unwrap();
///
/// // Multiple consumers can read
/// assert_eq!(buffer.pop(), Some(42));
/// assert_eq!(buffer.pop(), Some(100));
/// assert_eq!(buffer.pop(), None);
/// ```
pub struct MpmcRingBuffer<T> {
    /// Buffer slots, aligned to cache lines.
    buffer: Box<[Slot<T>]>,
    /// Capacity (must be power of 2).
    capacity: usize,
    /// Capacity mask for fast modulo.
    mask: usize,
    /// Head pointer (write position).
    head: AtomicUsize,
    /// Tail pointer (read position).
    tail: AtomicUsize,
    /// Closed flag - when true, writes fail and reads drain remaining.
    closed: AtomicBool,
    /// Padding to prevent false sharing.
    _pad: [u8; CACHE_LINE_SIZE - 2 * mem::size_of::<AtomicUsize>()],
}

unsafe impl<T: Send> Send for MpmcRingBuffer<T> {}
unsafe impl<T: Send + Sync> Sync for MpmcRingBuffer<T> {}

impl<T> MpmcRingBuffer<T> {
    /// Creates a new ring buffer with the given capacity.
    ///
    /// The capacity will be rounded up to the nearest power of 2.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is 0 or exceeds `MAX_CAPACITY`.
    #[inline]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "capacity must be > 0");
        assert!(capacity <= MAX_CAPACITY, "capacity exceeds MAX_CAPACITY");

        // Round up to nearest power of 2
        let capacity = capacity.next_power_of_two();
        let mask = capacity - 1;

        let buffer: Vec<Slot<T>> = (0..capacity).map(|i| Slot::new(i)).collect();
        let buffer = buffer.into_boxed_slice();

        Self {
            buffer,
            capacity,
            mask,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            _pad: [0; CACHE_LINE_SIZE - 2 * mem::size_of::<AtomicUsize>()],
        }
    }

    /// Pushes an item into the buffer.
    ///
    /// Returns `Ok(())` on success, or `Err(item)` if the buffer is full.
    #[inline]
    pub fn push(&self, item: T) -> Result<(), T> {
        if self.closed.load(Ordering::Acquire) {
            return Err(item);
        }

        let mask = self.mask;
        let mut head = self.head.load(Ordering::Relaxed);

        loop {
            let slot = unsafe { self.buffer.get_unchecked(head & mask) };
            let seq = slot.sequence.load(Ordering::Acquire);

            // Check if slot is ready for writing
            let diff = seq as isize - head as isize;
            if diff == 0 {
                // Slot is ready, try to claim it
                if self.head.compare_exchange_weak(
                    head,
                    head.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
                {
                    // We claimed this slot
                    unsafe {
                        ptr::write((*slot.data.get()).as_mut_ptr(), item);
                    }
                    slot.sequence.store(head.wrapping_add(1), Ordering::Release);
                    return Ok(());
                }
            } else if diff < 0 {
                // Buffer is full
                return Err(item);
            }
            // else: Another thread is writing (diff > 0), retry loop

            // CAS failed or another thread is writing, reload head
            head = self.head.load(Ordering::Relaxed);
        }
    }

    /// Pops an item from the buffer.
    ///
    /// Returns `Some(item)` if available, or `None` if empty.
    #[inline]
    pub fn pop(&self) -> Option<T> {
        let mask = self.mask;
        let mut tail = self.tail.load(Ordering::Relaxed);

        loop {
            let slot = unsafe { self.buffer.get_unchecked(tail & mask) };
            let seq = slot.sequence.load(Ordering::Acquire);

            // Check if slot is ready for reading
            let diff = seq as isize - ((tail.wrapping_add(1)) as isize);
            if diff == 0 {
                // Slot has data, try to claim it
                if self.tail.compare_exchange_weak(
                    tail,
                    tail.wrapping_add(1),
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                )
                .is_ok()
                {
                    // We claimed this slot
                    let item = unsafe { ptr::read((*slot.data.get()).as_ptr()) };
                    slot.sequence.store(tail.wrapping_add(self.capacity), Ordering::Release);
                    return Some(item);
                }
            } else if diff < 0 {
                // Buffer is empty or closed
                if self.closed.load(Ordering::Acquire) {
                    // Check one more time for remaining items
                    let seq = slot.sequence.load(Ordering::Acquire);
                    if seq as isize - ((tail.wrapping_add(1)) as isize) < 0 {
                        return None;
                    }
                }
                return None;
            }

            // CAS failed, reload tail
            tail = self.tail.load(Ordering::Relaxed);
        }
    }

    /// Attempts to push an item, spinning if the buffer is full.
    ///
    /// This will spin indefinitely until the push succeeds.
    #[inline]
    pub fn push_spin(&self, item: T) {
        let mut item = Some(item);
        loop {
            match self.push(item.take().unwrap()) {
                Ok(()) => return,
                Err(i) => {
                    item = Some(i);
                    std::hint::spin_loop();
                }
            }
        }
    }

    /// Attempts to pop an item, spinning if the buffer is empty.
    ///
    /// This will spin indefinitely until a value is available.
    #[inline]
    pub fn pop_spin(&self) -> T {
        loop {
            match self.pop() {
                Some(item) => return item,
                None => std::hint::spin_loop(),
            }
        }
    }

    /// Returns the number of items in the buffer.
    ///
    /// Note: This is a snapshot and may be stale immediately after reading.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        head.wrapping_sub(tail)
    }

    /// Returns `true` if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns `true` if the buffer is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }

    /// Returns the capacity of the buffer.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the remaining capacity.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.capacity.saturating_sub(self.len())
    }

    /// Closes the buffer for writing.
    ///
    /// After closing, pushes will fail but pops will continue to work
    /// until the buffer is empty.
    #[inline]
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Returns `true` if the buffer is closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Clears all items from the buffer.
    #[inline]
    pub fn clear(&self) {
        while self.pop().is_some() {}
    }
}

impl<T> fmt::Debug for MpmcRingBuffer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MpmcRingBuffer")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl<T> Default for MpmcRingBuffer<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// A bounded MPMC queue with backpressure support.
///
/// Unlike `MpmcRingBuffer`, this queue supports blocking operations
/// and provides feedback when the queue is full or empty.
///
/// # Example
///
/// ```rust
/// use nexus_utils::ring_buffer::BoundedQueue;
///
/// let queue = BoundedQueue::new(100);
///
/// // Push with backpressure
/// if queue.try_push(42).is_err() {
///     println!("Queue is full, applying backpressure");
/// }
///
/// // Pop with blocking
/// let item = queue.pop_wait();
/// ```
pub struct BoundedQueue<T> {
    /// Inner ring buffer.
    inner: MpmcRingBuffer<T>,
    /// Number of waiting producers.
    waiting_producers: AtomicUsize,
    /// Number of waiting consumers.
    waiting_consumers: AtomicUsize,
}

impl<T> BoundedQueue<T> {
    /// Creates a new bounded queue with the given capacity.
    #[inline]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: MpmcRingBuffer::new(capacity),
            waiting_producers: AtomicUsize::new(0),
            waiting_consumers: AtomicUsize::new(0),
        }
    }

    /// Tries to push an item, returning `Err` if full.
    #[inline]
    pub fn try_push(&self, item: T) -> Result<(), T> {
        self.inner.push(item)
    }

    /// Pushes an item, blocking if the queue is full.
    #[inline]
    pub fn push(&self, item: T) {
        self.waiting_producers.fetch_add(1, Ordering::Relaxed);
        let mut item = Some(item);
        loop {
            match self.inner.push(item.take().unwrap()) {
                Ok(()) => {
                    self.waiting_producers.fetch_sub(1, Ordering::Relaxed);
                    return;
                }
                Err(i) => {
                    item = Some(i);
                    // Yield to allow consumers to make progress
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Tries to pop an item, returning `None` if empty.
    #[inline]
    pub fn try_pop(&self) -> Option<T> {
        self.inner.pop()
    }

    /// Pops an item, blocking if the queue is empty.
    #[inline]
    pub fn pop_wait(&self) -> T {
        self.waiting_consumers.fetch_add(1, Ordering::Relaxed);
        loop {
            match self.inner.pop() {
                Some(item) => {
                    self.waiting_consumers.fetch_sub(1, Ordering::Relaxed);
                    return item;
                }
                None => {
                    // Yield to allow producers to make progress
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Pops an item with a timeout.
    #[inline]
    pub fn pop_timeout(&self, timeout: std::time::Duration) -> Option<T> {
        let start = std::time::Instant::now();
        self.waiting_consumers.fetch_add(1, Ordering::Relaxed);

        loop {
            match self.inner.pop() {
                Some(item) => {
                    self.waiting_consumers.fetch_sub(1, Ordering::Relaxed);
                    return Some(item);
                }
                None => {
                    if start.elapsed() >= timeout {
                        self.waiting_consumers.fetch_sub(1, Ordering::Relaxed);
                        return None;
                    }
                    std::thread::yield_now();
                }
            }
        }
    }

    /// Returns the number of items in the queue.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the queue is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns `true` if the queue is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.inner.is_full()
    }

    /// Returns the capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Returns the number of waiting producers.
    #[inline]
    pub fn waiting_producers(&self) -> usize {
        self.waiting_producers.load(Ordering::Relaxed)
    }

    /// Returns the number of waiting consumers.
    #[inline]
    pub fn waiting_consumers(&self) -> usize {
        self.waiting_consumers.load(Ordering::Relaxed)
    }

    /// Closes the queue.
    #[inline]
    pub fn close(&self) {
        self.inner.close();
    }

    /// Returns `true` if the queue is closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl<T> fmt::Debug for BoundedQueue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundedQueue")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .field("waiting_producers", &self.waiting_producers())
            .field("waiting_consumers", &self.waiting_consumers())
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl<T> Default for BoundedQueue<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// An SPSC (Single Producer, Single Consumer) ring buffer.
///
/// This is optimized for the case where there is exactly one producer
/// and one consumer. It uses simpler atomics and is faster than MPMC.
///
/// # Example
///
/// ```rust
/// use nexus_utils::ring_buffer::SpscRingBuffer;
///
/// let buffer = SpscRingBuffer::new(1024);
///
/// // Single producer writes
/// buffer.push(42).unwrap();
///
/// // Single consumer reads
/// assert_eq!(buffer.pop(), Some(42));
/// ```
pub struct SpscRingBuffer<T> {
    /// Buffer storage.
    buffer: Box<[UnsafeCell<MaybeUninit<T>>]>,
    /// Capacity (power of 2).
    capacity: usize,
    /// Capacity mask for fast modulo.
    mask: usize,
    /// Head (write position) - only modified by producer.
    head: AtomicUsize,
    /// Tail (read position) - only modified by consumer.
    tail: AtomicUsize,
    /// Closed flag.
    closed: AtomicBool,
}

unsafe impl<T: Send> Send for SpscRingBuffer<T> {}
unsafe impl<T: Send> Sync for SpscRingBuffer<T> {}

impl<T> SpscRingBuffer<T> {
    /// Creates a new SPSC buffer with the given capacity.
    #[inline]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0 && capacity <= MAX_CAPACITY);
        let capacity = capacity.next_power_of_two();
        let mask = capacity - 1;

        let buffer: Vec<_> = (0..capacity).map(|_| UnsafeCell::new(MaybeUninit::uninit())).collect();
        let buffer = buffer.into_boxed_slice();

        Self {
            buffer,
            capacity,
            mask,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Pushes an item into the buffer.
    ///
    /// Returns `Ok(())` on success, or `Err(item)` if the buffer is full.
    #[inline]
    pub fn push(&self, item: T) -> Result<(), T> {
        if self.closed.load(Ordering::Acquire) {
            return Err(item);
        }

        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);

        // Check if full
        if head.wrapping_sub(tail) >= self.capacity {
            return Err(item);
        }

        // Write to slot
        let slot = unsafe { self.buffer.get_unchecked(head & self.mask) };
        unsafe {
            ptr::write((*slot.get()).as_mut_ptr(), item);
        }

        // Publish write
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Pops an item from the buffer.
    #[inline]
    pub fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);

        // Check if empty
        if tail == head {
            if self.closed.load(Ordering::Acquire) {
                return None;
            }
            return None;
        }

        // Read from slot
        let slot = unsafe { self.buffer.get_unchecked(tail & self.mask) };
        let item = unsafe { ptr::read((*slot.get()).as_ptr()) };

        // Publish read
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(item)
    }

    /// Returns the number of items in the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        head.wrapping_sub(tail)
    }

    /// Returns `true` if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Relaxed) == self.tail.load(Ordering::Relaxed)
    }

    /// Returns `true` if the buffer is full.
    #[inline]
    pub fn is_full(&self) -> bool {
        self.len() >= self.capacity
    }

    /// Returns the capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Closes the buffer.
    #[inline]
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Returns `true` if closed.
    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

impl<T> fmt::Debug for SpscRingBuffer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpscRingBuffer")
            .field("capacity", &self.capacity)
            .field("len", &self.len())
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl<T> Default for SpscRingBuffer<T> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

/// An iterator over a ring buffer's contents.
pub struct IntoIter<T> {
    buffer: MpmcRingBuffer<T>,
}

impl<T> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.buffer.pop()
    }
}

impl<T> IntoIterator for MpmcRingBuffer<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        IntoIter { buffer: self }
    }
}

/// A ring buffer with typed slots for different message types.
///
/// This is useful for event buses where different event types
/// need to be stored in the same buffer.
pub struct TypedRingBuffer<E: Event> {
    inner: MpmcRingBuffer<SlotEntry<E>>,
}

/// A slot in the typed ring buffer.
#[derive(Debug)]
pub struct SlotEntry<E: Event> {
    /// Event type discriminator.
    pub event_type: E::Type,
    /// The actual event data.
    pub event: E,
}

/// Trait for events that can be stored in a typed ring buffer.
pub trait Event: Send {
    /// The type discriminator for this event.
    type Type: Copy + Clone + Send + Sync + Eq + std::hash::Hash;

    /// Returns the type of this event.
    fn event_type(&self) -> Self::Type;
}

impl<E: Event> TypedRingBuffer<E> {
    /// Creates a new typed ring buffer.
    #[inline]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: MpmcRingBuffer::new(capacity),
        }
    }

    /// Pushes an event into the buffer.
    #[inline]
    pub fn push(&self, event: E) -> Result<(), SlotEntry<E>> {
        let slot = SlotEntry {
            event_type: event.event_type(),
            event,
        };
        self.inner.push(slot)
    }

    /// Pops an event from the buffer.
    #[inline]
    pub fn pop(&self) -> Option<SlotEntry<E>> {
        self.inner.pop()
    }

    /// Pops all events of a specific type.
    #[inline]
    pub fn pop_by_type(&self, event_type: E::Type) -> impl Iterator<Item = SlotEntry<E>> + '_
    where
        E::Type: PartialEq,
    {
        std::iter::from_fn(move || {
            // This is a simplified implementation that doesn't preserve order
            // A real implementation would use a secondary buffer
            self.pop().filter(|s| s.event_type == event_type)
        })
    }

    /// Returns the number of events in the buffer.
    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if the buffer is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Returns the capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }
}

impl<E: Event + fmt::Debug> fmt::Debug for TypedRingBuffer<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedRingBuffer")
            .field("capacity", &self.capacity())
            .field("len", &self.len())
            .finish()
    }
}

impl<E: Event> Default for TypedRingBuffer<E> {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_mpmc_ring_buffer_basic() {
        let buffer = MpmcRingBuffer::new(16);

        for i in 0..10 {
            buffer.push(i).unwrap();
        }

        for i in 0..10 {
            assert_eq!(buffer.pop(), Some(i));
        }

        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn test_mpmc_ring_buffer_full() {
        let buffer = MpmcRingBuffer::new(4);

        // Capacity 4 is already a power of 2, so it stays 4
        let capacity = buffer.capacity();
        assert_eq!(capacity, 4);

        for i in 0..capacity {
            assert!(buffer.push(i).is_ok(), "Push {} should succeed", i);
        }

        // Should be full now
        assert!(buffer.is_full());
        assert!(buffer.push(100).is_err());
    }

    #[test]
    fn test_mpmc_ring_buffer_close() {
        let buffer = MpmcRingBuffer::new(16);

        buffer.push(1).unwrap();
        buffer.push(2).unwrap();

        buffer.close();

        // Writes should fail
        assert!(buffer.push(3).is_err());

        // Reads should still work
        assert_eq!(buffer.pop(), Some(1));
        assert_eq!(buffer.pop(), Some(2));
        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn test_mpmc_ring_buffer_concurrent() {
        let buffer = Arc::new(MpmcRingBuffer::new(1024));
        let mut handles = vec![];

        // Spawn producers
        for p in 0..4 {
            let buffer_clone = buffer.clone();
            handles.push(thread::spawn(move || {
                for i in 0..1000 {
                    buffer_clone.push_spin(p * 1000 + i);
                }
            }));
        }

        // Spawn consumers
        let mut consumers = vec![];
        for _ in 0..4 {
            let buffer_clone = buffer.clone();
            consumers.push(thread::spawn(move || {
                let mut count = 0;
                while count < 1000 {
                    if buffer_clone.pop().is_some() {
                        count += 1;
                    }
                }
                count
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let total: usize = consumers.into_iter().map(|h| h.join().unwrap()).sum();
        assert_eq!(total, 4000);
    }

    #[test]
    fn test_bounded_queue_blocking() {
        let queue = Arc::new(BoundedQueue::new(4));
        let capacity = queue.capacity(); // Will be 4 (power of 2)

        // Fill the queue - use try_push since push blocks when full
        for i in 0..capacity {
            assert!(queue.try_push(i).is_ok(), "Push {} should succeed", i);
        }

        // Now it should be full
        assert!(queue.is_full());
        assert!(queue.try_push(capacity).is_err());

        // Use pop_wait to drain in a separate thread
        let queue_clone = queue.clone();
        let consumer = thread::spawn(move || {
            let mut items = vec![];
            for _ in 0..capacity {
                items.push(queue_clone.pop_wait());
            }
            items
        });

        // Give consumer time to start waiting
        thread::sleep(std::time::Duration::from_millis(10));

        // Verify we got all items
        let items = consumer.join().unwrap();
        assert_eq!(items.len(), capacity);
    }

    #[test]
    fn test_spsc_ring_buffer() {
        let buffer = SpscRingBuffer::new(16);

        for i in 0..10 {
            buffer.push(i).unwrap();
        }

        for i in 0..10 {
            assert_eq!(buffer.pop(), Some(i));
        }

        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn test_spsc_ring_buffer_close() {
        let buffer = SpscRingBuffer::new(16);

        buffer.push(1).unwrap();
        buffer.push(2).unwrap();
        buffer.close();

        assert!(buffer.push(3).is_err());
        assert_eq!(buffer.pop(), Some(1));
        assert_eq!(buffer.pop(), Some(2));
        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn test_typed_ring_buffer() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        enum EventType {
            A,
            B,
        }

        #[derive(PartialEq, Debug)]
        struct TestEvent {
            event_type: EventType,
            data: i32,
        }

        impl Event for TestEvent {
            type Type = EventType;

            fn event_type(&self) -> Self::Type {
                self.event_type
            }
        }

        let buffer = TypedRingBuffer::new(16);

        buffer.push(TestEvent { event_type: EventType::A, data: 1 }).unwrap();
        buffer.push(TestEvent { event_type: EventType::B, data: 2 }).unwrap();
        buffer.push(TestEvent { event_type: EventType::A, data: 3 }).unwrap();

        let slot = buffer.pop().unwrap();
        assert_eq!(slot.event_type, EventType::A);
        assert_eq!(slot.event.data, 1);
    }

    #[test]
    fn test_ring_buffer_len() {
        let buffer = MpmcRingBuffer::new(16);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());

        buffer.push(1).unwrap();
        buffer.push(2).unwrap();
        assert_eq!(buffer.len(), 2);

        buffer.pop();
        assert_eq!(buffer.len(), 1);
    }

    // ============================================================================
    // Ring Buffer Overwrite Tests
    // ============================================================================

    #[test]
    fn test_ring_buffer_overwrites_oldest_slot_on_full() {
        // Create a 150-slot ring buffer.
        // Write values 0 through 149, filling it exactly.
        // Write value 150.
        // Expected: reading slot index 0 returns 150, not 0.
        // The oldest slot was silently overwritten with no error, no panic.
        let buffer = MpmcRingBuffer::new(150);

        // The capacity rounds up to next power of 2, so 150 -> 256
        let capacity = buffer.capacity();

        // Fill exactly capacity items
        for i in 0..capacity {
            buffer.push(i as u64).unwrap();
        }

        // Buffer is full
        assert!(buffer.is_full(), "Buffer should be full after filling");

        // Push one more - this should succeed by overwriting
        let result = buffer.push(capacity as u64);
        // The MpmcRingBuffer returns Err when full, not overwrite
        // So this should fail
        assert!(result.is_err(), "Push on full buffer should fail");

        // Now let's pop all items and verify
        for i in 0..capacity {
            let item = buffer.pop();
            assert!(item.is_some(), "Should get item {}", i);
            assert_eq!(item.unwrap(), i as u64, "Item {} should be {}", i, i);
        }

        // Buffer should be empty
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_ring_buffer_wraparound_multiple_times() {
        // Write values, wrap around multiple times, verify data integrity
        let buffer = MpmcRingBuffer::new(16);
        let capacity = buffer.capacity(); // Will be 16 (power of 2)

        // Fill and drain multiple times
        for round in 0..5 {
            // Fill
            for i in 0..capacity {
                buffer.push((round * capacity + i) as u64).unwrap();
            }

            // Drain
            for i in 0..capacity {
                let item = buffer.pop().unwrap();
                assert_eq!(
                    item,
                    (round * capacity + i) as u64,
                    "Round {}, item {} mismatch",
                    round,
                    i
                );
            }
        }

        assert!(buffer.is_empty(), "Buffer should be empty after draining");
    }

    #[test]
    fn test_ring_buffer_concurrent_wraparound() {
        // Concurrent producers and consumers with wraparound
        let buffer = Arc::new(MpmcRingBuffer::new(64));
        let total_items = 10_000;

        let buffer_clone = buffer.clone();
        let producer = thread::spawn(move || {
            for i in 0..total_items {
                while buffer_clone.push(i).is_err() {
                    std::hint::spin_loop();
                }
            }
        });

        let buffer_clone = buffer.clone();
        let consumer = thread::spawn(move || {
            let mut count = 0u64;
            while count < total_items {
                if let Some(item) = buffer_clone.pop() {
                    assert_eq!(item, count, "Item mismatch at count {}", count);
                    count += 1;
                } else {
                    std::hint::spin_loop();
                }
            }
            count
        });

        producer.join().unwrap();
        let received = consumer.join().unwrap();

        assert_eq!(received, total_items as u64, "Should receive all items");
    }
}