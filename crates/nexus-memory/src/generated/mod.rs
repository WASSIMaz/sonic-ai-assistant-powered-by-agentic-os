//! GENERATED module — layout.rs is emitted by build.rs.
//!
//! This module contains constants computed from nexus_layout.toml:
//! - SCHEMA_HASH: SHA-256 hash of the layout configuration
//! - Region sizes (IMMUTABLE_SIZE, APPEND_ONLY_SIZE, etc.)
//! - Slot counts and sizes for slab allocators
//! - Computed offsets for memory regions
//!
//! DO NOT EDIT layout.rs manually. Run `cargo build` to regenerate.

pub mod layout;

pub use layout::*;