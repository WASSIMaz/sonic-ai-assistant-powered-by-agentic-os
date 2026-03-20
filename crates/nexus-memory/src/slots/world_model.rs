//! world_model.rs — WorldModelScalars fast-path #[repr(C)] struct.
//!
//! PURPOSE (Layer 4, file #11 from plan):
//! The scalar fast-path struct stored in the IMMUTABLE mmap region.
//! Contains only the fields that Rust components read at high frequency
//! without going through Python. All fields are fixed-size primitives.
//!
//! Read by L2 Watchdog, EnvelopeMonitor, L1 Health Watchdog without the GIL.
//! All fields are std::sync::atomic types for lockless multi-thread reads.
//!
//! Stored at offset 0 of the live staging buffer in the IMMUTABLE region.
//! #[repr(C)] guarantees layout matches _layout.py's ctypes.Structure.

use std::sync::atomic::{AtomicU64, AtomicU32, AtomicU8};

/// The scalar fast-path struct stored in the IMMUTABLE mmap region.
/// Contains only the fields that Rust components read at high frequency
/// without going through Python. All fields are fixed-size primitives.
///
/// # Safety
/// #[repr(C)] guarantees the layout matches _layout.py's ctypes.Structure.
/// The compile-time assert below catches any accidental size change.
#[repr(C)]
pub struct WorldModelScalars {
    /// Monotonic timestamp of the last Oracle poll (nanoseconds).
    pub timestamp_ns: AtomicU64,

    /// PID of the currently active window's process.
    pub active_pid: AtomicU32,

    /// CPU utilization as f32 bits (use f32::from_bits() to read).
    /// Stored as AtomicU32 because AtomicF32 is not stable.
    pub cpu_pct_bits: AtomicU32,

    /// Physical memory currently in use, megabytes.
    pub memory_used_mb: AtomicU32,

    /// Free disk space on primary volume as f32 bits (gigabytes).
    pub disk_free_gb_bits: AtomicU32,

    /// Network reachability: 0=unreachable, 1=reachable.
    pub network_reachable: AtomicU8,

    /// Padding to 64 bytes (one cache line).
    /// Layout: AtomicU64(8) + AtomicU32(4) + AtomicU32(4) + AtomicU32(4) + AtomicU32(4) + AtomicU8(1) = 25 bytes
    /// _pad = 64 - 25 = 39 bytes.
    pub _pad: [u8; 39],
}

const _: () = assert!(
    std::mem::size_of::<WorldModelScalars>() == 64,
    "WorldModelScalars must be exactly 64 bytes"
);
