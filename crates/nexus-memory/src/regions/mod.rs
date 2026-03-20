//! regions — Memory region implementations for nexus-memory.
//!
//! This module provides the four hardware-backed memory regions:
//! - `immutable_region`: Ping-pong mmap with mprotect seal for TOCTOU-safe writes
//! - `append_only_region`: Observation ring + event log buffer
//! - `mutable_region`: RwLock per slot with Lamport clock advancement
//! - `compute_region`: Bus-coordinated matrix writes (no lock)

pub mod append_only_region;
pub mod compute_region;
pub mod immutable_region;
pub mod mutable_region;

pub use append_only_region::AppendOnlyRegion;
pub use compute_region::ComputeRegion;
pub use immutable_region::ImmutableRegion;
pub use mutable_region::MutableRegion;
