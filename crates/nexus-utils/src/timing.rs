//! # Timing Utilities for NEXUS Agentic OS
//!
//! This module provides high-resolution timing primitives for:
//!
//! - **Heartbeat timers**: Monotonic interval timers for kernel health monitoring
//! - **Drift-corrected timers**: Timers that compensate for clock drift
//! - **Interval timers**: Precise periodic timers for the 200ms Oracle poll cycle
//! - **Deadline tracking**: Deadline enforcement for goal execution
//!
//! ## Design Principles
//!
//! 1. **Drift Correction**: All periodic timers compensate for accumulated drift
//! 2. **Monotonicity**: Timestamps are always monotonic, never going backwards
//! 3. **Zero Allocation**: Hot-path timing operations never allocate
//! 4. **High Resolution**: Uses `Instant::now()` for nanosecond precision

use std::fmt;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::atomic::AtomicTimestamp;

/// Nanoseconds per microsecond
pub const NANOS_PER_MICRO: u64 = 1_000;
/// Nanoseconds per millisecond
pub const NANOS_PER_MILLI: u64 = 1_000_000;
/// Nanoseconds per second
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
/// Nanoseconds per minute
pub const NANOS_PER_MINUTE: u64 = 60 * NANOS_PER_SEC;
/// Nanoseconds per hour
pub const NANOS_PER_HOUR: u64 = 60 * NANOS_PER_MINUTE;

/// The default interval for Oracle polling (200ms).
pub const ORACLE_POLL_INTERVAL_MS: u64 = 200;

/// The default interval for kernel heartbeat (1s).
pub const HEARTBEAT_INTERVAL_MS: u64 = 1_000;

/// The default stall threshold for L2 monitor (3s).
pub const STALL_THRESHOLD_MS: u64 = 3_000;

/// A monotonic timestamp representing a point in time.
///
/// This timestamp is guaranteed to be monotonic - it never goes backwards
/// even if the system clock is adjusted. Internally uses `Instant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    /// Nanoseconds since some unspecified epoch (monotonic).
    pub nanos: u64,
}

impl Timestamp {
    /// Creates a new timestamp from nanoseconds.
    #[inline]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self { nanos }
    }

    /// Creates a new timestamp from microseconds.
    #[inline]
    pub const fn from_micros(micros: u64) -> Self {
        Self {
            nanos: micros * NANOS_PER_MICRO,
        }
    }

    /// Creates a new timestamp from milliseconds.
    #[inline]
    pub const fn from_millis(millis: u64) -> Self {
        Self {
            nanos: millis * NANOS_PER_MILLI,
        }
    }

    /// Creates a new timestamp from seconds.
    #[inline]
    pub const fn from_secs(secs: u64) -> Self {
        Self {
            nanos: secs * NANOS_PER_SEC,
        }
    }

    /// Creates a timestamp from a `Duration`.
    #[inline]
    pub fn from_duration(duration: Duration) -> Self {
        Self {
            nanos: duration.as_nanos() as u64,
        }
    }

    /// Creates a timestamp representing now (monotonic).
    #[inline]
    pub fn now() -> Self {
        // Use a lazy-initialized base instant for monotonicity
        static BASE: once_cell::sync::Lazy<Instant> = once_cell::sync::Lazy::new(Instant::now);
        Self {
            nanos: BASE.elapsed().as_nanos() as u64,
        }
    }

    /// Returns the timestamp as nanoseconds.
    #[inline]
    pub const fn as_nanos(&self) -> u64 {
        self.nanos
    }

    /// Returns the timestamp as microseconds (truncated).
    #[inline]
    pub const fn as_micros(&self) -> u64 {
        self.nanos / NANOS_PER_MICRO
    }

    /// Returns the timestamp as milliseconds (truncated).
    #[inline]
    pub const fn as_millis(&self) -> u64 {
        self.nanos / NANOS_PER_MILLI
    }

    /// Returns the timestamp as seconds (truncated).
    #[inline]
    pub const fn as_secs(&self) -> u64 {
        self.nanos / NANOS_PER_SEC
    }

    /// Returns the fractional seconds part in nanoseconds.
    #[inline]
    pub const fn subsec_nanos(&self) -> u64 {
        self.nanos % NANOS_PER_SEC
    }

    /// Returns the fractional seconds part in milliseconds.
    #[inline]
    pub const fn subsec_millis(&self) -> u64 {
        (self.nanos % NANOS_PER_SEC) / NANOS_PER_MILLI
    }

    /// Converts to a `Duration`.
    #[inline]
    pub const fn as_duration(&self) -> Duration {
        Duration::from_nanos(self.nanos)
    }

    /// Returns the elapsed time since this timestamp.
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Self::now().as_duration().saturating_sub(self.as_duration())
    }

    /// Returns the elapsed time in milliseconds since this timestamp.
    #[inline]
    pub fn elapsed_millis(&self) -> u64 {
        self.elapsed().as_millis() as u64
    }

    /// Returns the elapsed time in microseconds since this timestamp.
    #[inline]
    pub fn elapsed_micros(&self) -> u64 {
        self.elapsed().as_micros() as u64
    }

    /// Returns `true` if this timestamp is zero (epoch).
    #[inline]
    pub const fn is_zero(&self) -> bool {
        self.nanos == 0
    }

    /// Returns `true` if this timestamp has elapsed.
    #[inline]
    pub fn is_elapsed(&self) -> bool {
        Self::now().nanos >= self.nanos
    }

    /// Adds a duration to this timestamp.
    #[inline]
    pub const fn add_duration(&self, duration: Duration) -> Self {
        Self {
            nanos: self.nanos.saturating_add(duration.as_nanos() as u64),
        }
    }

    /// Subtracts a duration from this timestamp, saturating at zero.
    #[inline]
    pub const fn sub_duration(&self, duration: Duration) -> Self {
        Self {
            nanos: self.nanos.saturating_sub(duration.as_nanos() as u64),
        }
    }

    /// Returns the duration between this timestamp and another.
    #[inline]
    pub const fn duration_since(&self, earlier: Timestamp) -> Duration {
        Duration::from_nanos(self.nanos.saturating_sub(earlier.nanos))
    }

    /// Returns the difference between timestamps (can be negative).
    #[inline]
    pub const fn diff(&self, other: Timestamp) -> i64 {
        self.nanos as i64 - other.nanos as i64
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self { nanos: 0 }
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Duration) -> Self::Output {
        self.add_duration(rhs)
    }
}

impl AddAssign<Duration> for Timestamp {
    #[inline]
    fn add_assign(&mut self, rhs: Duration) {
        self.nanos = self.nanos.saturating_add(rhs.as_nanos() as u64);
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Duration) -> Self::Output {
        self.sub_duration(rhs)
    }
}

impl SubAssign<Duration> for Timestamp {
    #[inline]
    fn sub_assign(&mut self, rhs: Duration) {
        self.nanos = self.nanos.saturating_sub(rhs.as_nanos() as u64);
    }
}

impl Sub for Timestamp {
    type Output = Duration;

    #[inline]
    fn sub(self, rhs: Self) -> Self::Output {
        self.duration_since(rhs)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.as_secs();
        let millis = self.subsec_millis();
        if secs > 0 {
            write!(f, "{}.{:03}s", secs, millis)
        } else {
            write!(f, "{}ms", self.as_millis())
        }
    }
}

/// A drift-corrected periodic timer.
///
/// This timer compensates for accumulated drift by tracking the actual
/// time elapsed versus expected time. The drift is measured and corrected
/// on each tick.
///
/// # Example
///
/// ```rust
/// use nexus_utils::timing::{DriftCorrectedTimer, Timestamp};
/// use std::time::Duration;
///
/// let mut timer = DriftCorrectedTimer::new(Duration::from_millis(200));
/// timer.start();
///
/// // Simulate work
/// std::thread::sleep(Duration::from_millis(50));
///
/// if timer.tick() {
///     // 200ms interval has elapsed
///     println!("Drift: {:?}", timer.drift());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct DriftCorrectedTimer {
    /// The configured interval.
    interval: Duration,
    /// The next expected fire time.
    next_fire: Timestamp,
    /// Total accumulated drift in nanoseconds.
    drift_ns: i64,
    /// Maximum tolerated drift before correction.
    max_drift_ns: u64,
    /// Total number of ticks.
    tick_count: u64,
    /// Whether the timer is currently running.
    running: bool,
}

impl DriftCorrectedTimer {
    /// Creates a new drift-corrected timer with the given interval.
    #[inline]
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_fire: Timestamp::from_nanos(0),
            drift_ns: 0,
            max_drift_ns: interval.as_nanos() as u64 / 10, // 10% tolerance
            tick_count: 0,
            running: false,
        }
    }

    /// Creates a new timer with a custom maximum drift tolerance.
    #[inline]
    pub fn with_max_drift(interval: Duration, max_drift: Duration) -> Self {
        Self {
            interval,
            next_fire: Timestamp::from_nanos(0),
            drift_ns: 0,
            max_drift_ns: max_drift.as_nanos() as u64,
            tick_count: 0,
            running: false,
        }
    }

    /// Starts or restarts the timer.
    #[inline]
    pub fn start(&mut self) {
        self.next_fire = Timestamp::now() + self.interval;
        self.drift_ns = 0;
        self.tick_count = 0;
        self.running = true;
    }

    /// Stops the timer.
    #[inline]
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Returns `true` if the interval has elapsed.
    ///
    /// If the interval has elapsed, this method advances the timer to the next
    /// interval and returns `true`. Otherwise returns `false`.
    #[inline]
    pub fn tick(&mut self) -> bool {
        if !self.running {
            return false;
        }

        let now = Timestamp::now();
        if now.nanos >= self.next_fire.nanos {
            // Calculate actual elapsed time since last fire
            let actual_elapsed_ns = (now.nanos - (self.next_fire.nanos - (self.interval.as_nanos() as u64))) as i64;
            let expected_ns = self.interval.as_nanos() as i64;

            // Track drift
            self.drift_ns += actual_elapsed_ns - expected_ns;

            // Advance to next fire time, compensating for drift
            let next_interval_ns = if self.drift_ns > 0 {
                // We're running late, use shorter interval to catch up
                (self.interval.as_nanos() as u64)
                    .saturating_sub(self.drift_ns.abs() as u64 / 10) // Gradual correction
            } else {
                // We're running early, use normal interval
                self.interval.as_nanos() as u64
            };

            self.next_fire = Timestamp::from_nanos(now.nanos + next_interval_ns);
            self.tick_count += 1;
            true
        } else {
            false
        }
    }

    /// Waits until the next tick, blocking the current thread.
    ///
    /// Returns the actual time waited, which may differ from the interval
    /// due to drift correction.
    #[inline]
    pub fn wait(&mut self) -> Duration {
        if !self.running {
            self.start();
        }

        let now = Timestamp::now();
        if now < self.next_fire {
            let wait_time = self.next_fire - now;
            std::thread::sleep(wait_time);
        }

        self.tick();

        let actual_wait = Timestamp::now() - now;
        actual_wait
    }

    /// Returns the remaining time until the next fire.
    #[inline]
    pub fn remaining(&self) -> Duration {
        if !self.running {
            return Duration::ZERO;
        }
        let now = Timestamp::now();
        if now >= self.next_fire {
            Duration::ZERO
        } else {
            self.next_fire - now
        }
    }

    /// Returns the accumulated drift.
    ///
    /// Positive drift means the timer is running late, negative means early.
    #[inline]
    pub fn drift(&self) -> Duration {
        Duration::from_nanos(self.drift_ns.abs() as u64)
    }

    /// Returns the total number of ticks since start.
    #[inline]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Resets the timer and clears accumulated drift.
    #[inline]
    pub fn reset(&mut self) {
        self.start();
    }

    /// Returns `true` if the timer is running.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Returns the configured interval.
    #[inline]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Returns the next fire time.
    #[inline]
    pub fn next_fire(&self) -> Timestamp {
        self.next_fire
    }

    /// Adjusts the interval dynamically.
    ///
    /// This is useful for adaptive polling intervals based on system load.
    #[inline]
    pub fn adjust_interval(&mut self, new_interval: Duration) {
        self.interval = new_interval;
        self.max_drift_ns = new_interval.as_nanos() as u64 / 10;
    }
}

/// A heartbeat timer for kernel health monitoring.
///
/// Provides a monotonic heartbeat that can be checked for staleness.
/// The L2 monitor checks if the heartbeat has been updated within the
/// expected interval; if not, the kernel is considered stalled.
///
/// # Example
///
/// ```rust
/// use nexus_utils::timing::HeartbeatTimer;
/// use std::time::Duration;
///
/// let heartbeat = HeartbeatTimer::new(Duration::from_secs(1));
/// heartbeat.beat(); // Call this every second in the kernel loop
///
/// if heartbeat.is_stalled(Duration::from_secs(3)) {
///     println!("Kernel heartbeat stalled!");
/// }
/// ```
#[derive(Debug)]
pub struct HeartbeatTimer {
    /// The last heartbeat timestamp.
    last_beat: AtomicTimestamp,
    /// The expected interval between beats.
    interval_ns: AtomicU64,
    /// Number of beats since creation.
    beat_count: AtomicU64,
    /// Whether the heartbeat is active.
    active: AtomicBool,
}

impl HeartbeatTimer {
    /// Creates a new heartbeat timer with the given expected interval.
    #[inline]
    pub fn new(interval: Duration) -> Self {
        Self {
            last_beat: AtomicTimestamp::now(),
            interval_ns: AtomicU64::new(interval.as_nanos() as u64),
            beat_count: AtomicU64::new(1),
            active: AtomicBool::new(true),
        }
    }

    /// Records a heartbeat.
    ///
    /// This should be called at the end of each kernel cycle.
    #[inline]
    pub fn beat(&self) {
        self.last_beat.update_to_now();
        self.beat_count.fetch_add(1, Ordering::Release);
    }

    /// Returns the time since the last heartbeat.
    #[inline]
    pub fn time_since_beat(&self) -> Duration {
        {
            let last = self.last_beat.load(Ordering::Acquire);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            std::time::Duration::from_nanos(now.saturating_sub(last))
        }
    }

    /// Returns the milliseconds since the last heartbeat.
    #[inline]
    pub fn millis_since_beat(&self) -> u64 {
        self.time_since_beat().as_millis() as u64
    }

    /// Checks if the heartbeat is stalled.
    ///
    /// A heartbeat is stalled if more than `threshold` time has passed
    /// since the last beat.
    #[inline]
    pub fn is_stalled(&self, threshold: Duration) -> bool {
        self.time_since_beat() > threshold
    }

    /// Returns the number of beats since creation.
    #[inline]
    pub fn beat_count(&self) -> u64 {
        self.beat_count.load(Ordering::Acquire)
    }

    /// Returns the expected interval.
    #[inline]
    pub fn interval(&self) -> Duration {
        Duration::from_nanos(self.interval_ns.load(Ordering::Acquire))
    }

    /// Updates the expected interval.
    #[inline]
    pub fn set_interval(&self, interval: Duration) {
        self.interval_ns.store(interval.as_nanos() as u64, Ordering::Release);
    }

    /// Deactivates the heartbeat.
    #[inline]
    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Reactivates the heartbeat and records a beat.
    #[inline]
    pub fn restart(&self) {
        self.beat();
        self.active.store(true, Ordering::Release);
    }

    /// Returns `true` if the heartbeat is active.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Returns the last beat timestamp.
    #[inline]
    pub fn last_beat(&self) -> Timestamp {
        Timestamp::from_nanos(self.last_beat.load(Ordering::Acquire))
    }

    /// Checks if the next beat is due (based on interval).
    #[inline]
    pub fn is_beat_due(&self) -> bool {
        self.time_since_beat() >= self.interval()
    }

    /// Resets the heartbeat count and timestamp.
    #[inline]
    pub fn reset(&self) {
        self.last_beat.update_to_now();
        self.beat_count.store(1, Ordering::Release);
    }
}

impl Default for HeartbeatTimer {
    fn default() -> Self {
        Self::new(Duration::from_secs(1))
    }
}

/// A deadline tracker for goal execution.
///
/// Tracks wall-clock deadlines and computes progress ratios
/// for the EnvelopeMonitor.
///

/// ```
#[derive(Debug, Clone)]
pub struct Deadline {
    /// The deadline timestamp (monotonic).
    deadline: Timestamp,
    /// The start timestamp.
    start: Timestamp,
    /// The total duration.
    total_duration: Duration,
}

impl Deadline {
    /// Creates a deadline from a specific timestamp.
    #[inline]
    pub fn at(deadline: Timestamp) -> Self {
        Self {
            deadline,
            start: Timestamp::now(),
            total_duration: Duration::ZERO,
        }
    }

    /// Creates a deadline relative to now.
    #[inline]
    pub fn from_now(duration: Duration) -> Self {
        let start = Timestamp::now();
        let deadline = start + duration;
        Self {
            deadline,
            start,
            total_duration: duration,
        }
    }

    /// Creates a deadline that never expires.
    #[inline]
    pub fn never() -> Self {
        Self {
            deadline: Timestamp::from_nanos(u64::MAX),
            start: Timestamp::now(),
            total_duration: Duration::MAX,
        }
    }

    /// Returns `true` if the deadline has expired.
    #[inline]
    pub fn is_expired(&self) -> bool {
        Timestamp::now() >= self.deadline
    }

    /// Returns the remaining time until the deadline.
    #[inline]
    pub fn remaining(&self) -> Duration {
        let now = Timestamp::now();
        if now >= self.deadline {
            Duration::ZERO
        } else {
            self.deadline - now
        }
    }

    /// Returns the remaining time in milliseconds.
    #[inline]
    pub fn remaining_millis(&self) -> u64 {
        self.remaining().as_millis() as u64
    }

    /// Returns the elapsed time since start.
    #[inline]
    pub fn elapsed(&self) -> Duration {
        Timestamp::now() - self.start
    }

    /// Returns the total duration of this deadline.
    #[inline]
    pub fn total_duration(&self) -> Duration {
        self.total_duration
    }

    /// Returns the progress ratio (elapsed / total).
    ///
    /// Returns `1.0` if expired, even if no duration was set.
    #[inline]
    pub fn progress_ratio(&self) -> f64 {
        if self.is_expired() {
            return 1.0;
        }
        if self.total_duration.is_zero() {
            return 0.0;
        }
        self.elapsed().as_nanos() as f64 / self.total_duration.as_nanos() as f64
    }

    /// Returns the remaining ratio (remaining / total).
    #[inline]
    pub fn remaining_ratio(&self) -> f64 {
        1.0 - self.progress_ratio()
    }

    /// Returns the start timestamp.
    #[inline]
    pub fn start(&self) -> Timestamp {
        self.start
    }

    /// Returns the deadline timestamp.
    #[inline]
    pub fn deadline(&self) -> Timestamp {
        self.deadline
    }

    /// Extends the deadline by the given duration.
    #[inline]
    pub fn extend(&mut self, duration: Duration) {
        self.deadline = self.deadline + duration;
        self.total_duration = self.deadline - self.start;
    }

    /// Returns a human-readable string representation.
    #[inline]
    pub fn format(&self) -> String {
        if self.is_expired() {
            "expired".to_string()
        } else {
            let remaining = self.remaining();
            if remaining.as_secs() > 0 {
                format!("{}.{:03}s remaining", remaining.as_secs(), remaining.subsec_millis())
            } else {
                format!("{}ms remaining", remaining.as_millis())
            }
        }
    }
}

impl Default for Deadline {
    fn default() -> Self {
        Self::never()
    }
}

impl fmt::Display for Deadline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.format())
    }
}

/// An adaptive interval timer that adjusts based on observed behavior.
///
/// This is used by FleetMonitor and EnvelopeMonitor to adjust polling
/// intervals based on goal phase or system load.
///
/// # Example
///
/// ```rust
/// use nexus_utils::timing::{AdaptiveTimer, Phase};
/// use std::time::Duration;
///
/// let mut timer = AdaptiveTimer::new(Duration::from_millis(500))
///     .with_phase_intervals(200, 500, 150);
///
/// timer.set_phase(Phase::Starting);
/// timer.start();
/// timer.tick(); // Uses 200ms interval
///
/// timer.set_phase(Phase::Executing);
/// timer.tick(); // Uses 500ms interval
/// ```
#[derive(Debug, Clone)]
pub struct AdaptiveTimer {
    /// Base interval (stored for reset capability).
    #[allow(dead_code)]
    base_interval: Duration,
    /// Current interval (adjusted).
    current_interval: Duration,
    /// Minimum interval.
    min_interval: Duration,
    /// Maximum interval.
    max_interval: Duration,
    /// Drift-corrected timer.
    timer: DriftCorrectedTimer,
    /// Current phase.
    phase: Phase,
    /// Phase-specific intervals.
    phase_intervals: PhaseIntervals,
    /// Stall detection state.
    stall_count: u32,
    /// Last progress value for stall detection.
    last_progress: f64,
}

/// Goal execution phases for adaptive timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Starting phase (first 20% of typical steps).
    Starting,
    /// Executing phase (20%-80% of typical steps).
    Executing,
    /// Completing phase (>80% of typical steps).
    Completing,
    /// Stalled phase (progress static for multiple cycles).
    Stalled,
}

impl Default for Phase {
    fn default() -> Self {
        Self::Starting
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Starting => write!(f, "STARTING"),
            Phase::Executing => write!(f, "EXECUTING"),
            Phase::Completing => write!(f, "COMPLETING"),
            Phase::Stalled => write!(f, "STALLED"),
        }
    }
}

/// Phase-specific intervals.
#[derive(Debug, Clone, Copy)]
struct PhaseIntervals {
    starting: Duration,
    executing: Duration,
    completing: Duration,
    stalled: Duration,
}

impl Default for PhaseIntervals {
    fn default() -> Self {
        Self {
            starting: Duration::from_millis(200),
            executing: Duration::from_millis(500),
            completing: Duration::from_millis(150),
            stalled: Duration::from_millis(100),
        }
    }
}

impl AdaptiveTimer {
    /// Creates a new adaptive timer with the given base interval.
    pub fn new(base_interval: Duration) -> Self {
        let timer = DriftCorrectedTimer::new(base_interval);
        Self {
            base_interval,
            current_interval: base_interval,
            min_interval: Duration::from_millis(50),
            max_interval: Duration::from_secs(5),
            timer,
            phase: Phase::Starting,
            phase_intervals: PhaseIntervals::default(),
            stall_count: 0,
            last_progress: 0.0,
        }
    }

    /// Sets the minimum interval.
    pub fn with_min_interval(mut self, min: Duration) -> Self {
        self.min_interval = min;
        self
    }

    /// Sets the maximum interval.
    pub fn with_max_interval(mut self, max: Duration) -> Self {
        self.max_interval = max;
        self
    }

    /// Sets phase-specific intervals.
    ///
    /// Arguments: starting_ms, executing_ms, completing_ms
    pub fn with_phase_intervals(mut self, starting_ms: u64, executing_ms: u64, completing_ms: u64) -> Self {
        self.phase_intervals = PhaseIntervals {
            starting: Duration::from_millis(starting_ms),
            executing: Duration::from_millis(executing_ms),
            completing: Duration::from_millis(completing_ms),
            stalled: Duration::from_millis(100),
        };
        self
    }

    /// Starts the timer.
    pub fn start(&mut self) {
        self.timer.start();
    }

    /// Returns `true` if a tick is due.
    pub fn tick(&mut self) -> bool {
        self.timer.tick()
    }

    /// Waits until the next tick.
    pub fn wait(&mut self) -> Duration {
        self.timer.wait()
    }

    /// Sets the current phase and adjusts interval accordingly.
    pub fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.current_interval = match phase {
            Phase::Starting => self.phase_intervals.starting,
            Phase::Executing => self.phase_intervals.executing,
            Phase::Completing => self.phase_intervals.completing,
            Phase::Stalled => self.phase_intervals.stalled,
        };
        self.timer.adjust_interval(self.current_interval);
    }

    /// Updates based on progress ratio (for stall detection).
    ///
    /// Returns `true` if a stall is detected.
    pub fn update_progress(&mut self, progress: f64) -> bool {
        let progress_delta = (progress - self.last_progress).abs();
        self.last_progress = progress;

        if progress_delta < 0.01 {
            self.stall_count += 1;
            if self.stall_count >= 3 {
                self.set_phase(Phase::Stalled);
                return true;
            }
        } else {
            self.stall_count = 0;
        }
        false
    }

    /// Returns the current phase.
    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// Returns the current interval.
    pub fn current_interval(&self) -> Duration {
        self.current_interval
    }

    /// Returns the remaining time until next tick.
    pub fn remaining(&self) -> Duration {
        self.timer.remaining()
    }

    /// Returns the accumulated drift.
    pub fn drift(&self) -> Duration {
        self.timer.drift()
    }

    /// Resets the timer.
    pub fn reset(&mut self) {
        self.timer.reset();
        self.phase = Phase::Starting;
        self.stall_count = 0;
        self.last_progress = 0.0;
    }
}

impl Default for AdaptiveTimer {
    fn default() -> Self {
        Self::new(Duration::from_millis(500))
    }
}

/// System time utilities.
pub mod system_time {
    use super::*;

    /// Returns the current system time as nanoseconds since Unix epoch.
    #[inline]
    pub fn now_nanos() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    /// Returns the current system time as milliseconds since Unix epoch.
    #[inline]
    pub fn now_millis() -> u64 {
        now_nanos() / NANOS_PER_MILLI
    }

    /// Returns the current system time as seconds since Unix epoch.
    #[inline]
    pub fn now_secs() -> u64 {
        now_nanos() / NANOS_PER_SEC
    }

    /// Returns the current system time as an ISO 8601 string.
    pub fn now_iso8601() -> String {
        let millis = now_millis();
        let secs = millis / 1000;
        let ms = millis % 1000;
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            1970 + secs / 31536000,
            (secs % 31536000) / 2592000,
            (secs % 2592000) / 86400,
            (secs % 86400) / 3600,
            (secs % 3600) / 60,
            secs % 60,
            ms
        )
    }
}

/// A rate limiter for controlling operation frequency.
///
/// Uses a token bucket algorithm to limit operations to a maximum rate.
#[derive(Debug)]
pub struct RateLimiter {
    /// Maximum tokens in the bucket.
    max_tokens: f64,
    /// Current tokens in the bucket.
    tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    /// Last refill time.
    last_refill: Instant,
}

impl RateLimiter {
    /// Creates a new rate limiter.
    ///
    /// # Arguments
    /// * `max_tokens` - Maximum burst capacity
    /// * `refill_rate` - Tokens added per second
    pub fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            max_tokens,
            tokens: max_tokens,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Creates a rate limiter for operations per second.
    pub fn per_second(ops: f64) -> Self {
        Self::new(ops, ops)
    }

    /// Creates a rate limiter for operations per minute.
    pub fn per_minute(ops: f64) -> Self {
        Self::new(ops, ops / 60.0)
    }

    /// Tries to acquire a token.
    ///
    /// Returns `true` if the operation should proceed.
    pub fn try_acquire(&mut self) -> bool {
        self.try_acquire_n(1.0)
    }

    /// Tries to acquire multiple tokens.
    pub fn try_acquire_n(&mut self, tokens: f64) -> bool {
        self.refill();
        if self.tokens >= tokens {
            self.tokens -= tokens;
            true
        } else {
            false
        }
    }

    /// Waits until a token is available.
    pub fn acquire(&mut self) {
        self.acquire_n(1.0)
    }

    /// Waits until multiple tokens are available.
    pub fn acquire_n(&mut self, tokens: f64) {
        loop {
            self.refill();
            if self.tokens >= tokens {
                self.tokens -= tokens;
                return;
            }
            let deficit = tokens - self.tokens;
            let wait_time = Duration::from_secs_f64(deficit / self.refill_rate);
            std::thread::sleep(wait_time);
        }
    }

    /// Refills tokens based on elapsed time.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
    }

    /// Returns the current token count.
    pub fn tokens(&self) -> f64 {
        self.tokens
    }

    /// Resets to full capacity.
    pub fn reset(&mut self) {
        self.tokens = self.max_tokens;
        self.last_refill = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timestamp_operations() {
        let t1 = Timestamp::from_millis(100);
        let t2 = Timestamp::from_millis(200);

        assert_eq!(t2.as_millis(), 200);
        assert_eq!(t2.as_secs(), 0);
        assert_eq!(t2.subsec_millis(), 200);

        let diff = t2 - t1;
        assert_eq!(diff, Duration::from_millis(100));

        let t3 = t1 + Duration::from_millis(50);
        assert_eq!(t3.as_millis(), 150);
    }

    #[test]
    fn test_drift_corrected_timer() {
        let mut timer = DriftCorrectedTimer::new(Duration::from_millis(50));
        timer.start();

        // First tick should fire immediately since we just started
        std::thread::sleep(Duration::from_millis(60));
        assert!(timer.tick());
        assert_eq!(timer.tick_count(), 1);
    }

    #[test]
    fn test_heartbeat_timer() {
        let heartbeat = HeartbeatTimer::new(Duration::from_millis(100));

        assert!(!heartbeat.is_stalled(Duration::from_millis(100)));
        assert!(heartbeat.is_active());

        std::thread::sleep(Duration::from_millis(150));
        assert!(heartbeat.is_stalled(Duration::from_millis(100)));

        heartbeat.beat();
        assert!(!heartbeat.is_stalled(Duration::from_millis(100)));
    }

    #[test]
    fn test_deadline() {
        let deadline = Deadline::from_now(Duration::from_millis(100));

        assert!(!deadline.is_expired());
        assert!(deadline.remaining() > Duration::from_millis(50));

        std::thread::sleep(Duration::from_millis(110));
        assert!(deadline.is_expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
        assert!((deadline.progress_ratio() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_adaptive_timer() {
        let mut timer = AdaptiveTimer::new(Duration::from_millis(500))
            .with_phase_intervals(200, 500, 150);

        timer.set_phase(Phase::Starting);
        assert_eq!(timer.current_interval(), Duration::from_millis(200));

        timer.set_phase(Phase::Executing);
        assert_eq!(timer.current_interval(), Duration::from_millis(500));

        timer.set_phase(Phase::Completing);
        assert_eq!(timer.current_interval(), Duration::from_millis(150));
    }

    #[test]
    fn test_rate_limiter() {
        let mut limiter = RateLimiter::per_second(10.0);

        // Should be able to acquire tokens initially
        assert!(limiter.try_acquire());

        // Drain the bucket
        for _ in 0..9 {
            assert!(limiter.try_acquire());
        }

        // Should now be empty
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_system_time() {
        let nanos = system_time::now_nanos();
        let millis = system_time::now_millis();
        let secs = system_time::now_secs();

        assert!(nanos > 0);
        assert!(millis > 0);
        assert!(secs > 0);
        assert!(nanos / 1_000_000 >= millis.saturating_sub(1));
    }
}