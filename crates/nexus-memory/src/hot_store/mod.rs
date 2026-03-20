//! hot_store — Hot-tier knowledge store for nexus-memory.
//!
//! This module provides:
//! - `world_model_store`: ArcSwap-backed WorldModel hot store
//! - `machine_profile_store`: Frozen MachineProfile with 5-minute refresh cycle
//! - `resident_embedding_model`: Single model instance with backend detection
//! - `curated_facts_store`: MEMORY.md loaded into frozen no-decay struct

pub mod curated_facts_store;
pub mod machine_profile_store;
pub mod resident_embedding_model;
pub mod world_model_store;

pub use world_model_store::WorldModelStore;
