//! allocator — Memory allocation primitives for nexus-memory.
//!
//! This module provides:
//! - `mmap`: Cross-platform anonymous shared memory allocation
//! - `slab`: Static pre-allocation slab allocator for typed slots
//!
//! See `protocols.rs` for the concurrency protocol documentation.

pub mod mmap;
pub mod slab;

pub use mmap::MmapRegion;
pub use slab::{SlabAllocator, slab_classes};
