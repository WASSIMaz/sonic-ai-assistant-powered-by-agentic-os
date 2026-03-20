//! slots — Slot struct definitions for nexus-memory.
//!
//! This module provides all slot types used in the TypedBuffer regions:
//! - `world_model`: WorldModelScalars fast-path struct
//! - `observation_ring`: ObservationSnapshot and ObservationRing
//! - `event_log_buffer`: EventRecord and EventLogBuffer
//! - `goal_states`: GoalStateEntry for GoalDependencyGraph
//! - `context_envelope`: ContextEnvelopeEntry for agent reasoning context
//! - `agent_registry`: AgentRegistryEntry with heartbeat and status
//! - `token_state`: TokenStateEntry for Input Surface Token
//! - `surface_assignments`: SurfaceAssignmentEntry mapping
//! - `capability_map`: CapabilityMapEntry routing table
//! - `token_debt_ledger`: TokenDebtEntry for IST accounting
//! - `machine_profile`: Frozen MachineProfile metadata reference
//! - `hnsw_index`: usearch::Index wrapper for ANN
//! - `embedding_cache`: DashMap-based LRU cache

pub mod agent_registry;
pub mod capability_map;
pub mod capability_matrix;
pub mod context_envelope;
pub mod embedding_cache;
pub mod event_log_buffer;
pub mod goal_states;
pub mod hnsw_index;
pub mod machine_profile;
pub mod observation_ring;
pub mod surface_assignments;
pub mod token_debt_ledger;
pub mod token_state;
pub mod world_model;

pub use observation_ring::{ObservationSnapshot, ObservationRing, ReadResult};
pub use goal_states::GoalStateEntry;
pub use context_envelope::ContextEnvelopeEntry;
pub use agent_registry::AgentRegistryEntry;
pub use token_state::TokenStateEntry;
pub use surface_assignments::SurfaceAssignmentEntry;
pub use capability_map::CapabilityMapEntry;
pub use token_debt_ledger::TokenDebtEntry;
pub use world_model::WorldModelScalars;
pub use machine_profile::MachineProfile;
pub use hnsw_index::HnswIndexHandle;
pub use embedding_cache::EmbeddingCache;
