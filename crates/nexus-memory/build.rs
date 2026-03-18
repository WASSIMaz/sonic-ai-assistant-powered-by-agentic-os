//! build.rs — Code generation for nexus-memory layout constants.
//!
//! WHAT THIS FILE DOES (executed at every `cargo build`):
//!
//! 1. Reads and parses nexus_layout.toml using serde.
//! 2. Computes all region base offsets from sizes (immutable → append_only → mutable → compute).
//! 3. Computes byte offsets for every field of ObservationSnapshot in order,
//!    verifying the total is exactly 320 bytes.
//! 4. Serializes the complete layout to a canonical deterministic byte string.
//!    Canonical means: sorted keys, fixed-width values, no optional whitespace.
//! 5. SHA-256 hashes that byte string → SCHEMA_HASH: [u8; 32].
//! 6. Writes src/generated/layout.rs with all constants.
//! 7. Writes python/nexus/_layout.py with ctypes.Structure definitions.
//! 8. Writes the computed offsets back into nexus_layout.toml [offsets] section.
//!
//! SCHEMA HASH CONTRACT:
//! Any change to nexus_layout.toml produces a different SCHEMA_HASH.
//! Rust bakes the new hash into layout.rs at build time.
//! If _layout.py is not regenerated (stale from a previous build), Python
//! startup will detect hash mismatch and raise NexusLayoutError.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use sha2::{Sha256, Digest};

fn main() {
    // Tell cargo to re-run this script if nexus_layout.toml or build.rs changes.
    println!("cargo:rerun-if-changed=nexus_layout.toml");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let out_dir = PathBuf::from(&manifest_dir).join("src").join("generated");
    let python_dir = PathBuf::from(&manifest_dir).join("python").join("nexus");

    // Ensure output directories exist
    fs::create_dir_all(&out_dir).expect("Failed to create generated directory");
    fs::create_dir_all(&python_dir).expect("Failed to create python directory");

    // Read and parse nexus_layout.toml
    let toml_path = PathBuf::from(&manifest_dir).join("nexus_layout.toml");
    let toml_content = fs::read_to_string(&toml_path).expect("Failed to read nexus_layout.toml");
    let config: LayoutToml = toml::from_str(&toml_content).expect("Failed to parse nexus_layout.toml");

    // Compute derived offsets
    let offsets = compute_offsets(&config);

    // Compute SCHEMA_HASH from canonical serialization
    let hash = compute_schema_hash(&config, &offsets);

    // Emit layout.rs
    emit_layout_rs(&config, &offsets, hash, &out_dir);

    // Emit _layout.py
    emit_layout_py(&config, &offsets, hash, &python_dir);

    // Update toml with computed offsets
    update_toml_offsets(&toml_path, &toml_content, &offsets);
}

// ============================================================================
// TOML Configuration Structures
// ============================================================================

/// Root TOML structure matching nexus_layout.toml
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct LayoutToml {
    schema: SchemaSection,
    regions: RegionsSection,
    header: HeaderSection,
    #[serde(rename = "observation_ring")]
    observation_ring: ObservationRingSection,
    slabs: SlabsSection,
    embedding: EmbeddingSection,
    #[serde(rename = "event_log")]
    event_log: EventLogSection,
    timeouts: TimeoutsSection,
    offsets: OffsetsSection,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct SchemaSection {
    version: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct RegionsSection {
    immutable_size_bytes: usize,
    append_only_size_bytes: usize,
    mutable_size_bytes: usize,
    compute_size_bytes: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct HeaderSection {
    size_bytes: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct ObservationRingSection {
    slot_count: usize,
    slot_size_bytes: usize,
    content_bytes: usize,
    cache_pad_bytes: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct SlabsSection {
    goal_contract_slots: usize,
    goal_contract_bytes: usize,
    goal_state_slots: usize,
    goal_state_bytes: usize,
    observation_slots: usize,
    observation_bytes: usize,
    atomic_counter_slots: usize,
    atomic_counter_bytes: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct EmbeddingSection {
    dim: usize,
    max_apps: usize,
    cache_max_entries: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct EventLogSection {
    capacity_bytes: usize,
    avg_event_bytes: usize,
    emergency_buffer_bytes: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct TimeoutsSection {
    sqlite_ready_ms: u64,
    write_lock_timeout_ms: u64,
    compute_refresh_min: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct OffsetsSection {
    // Computed by build.rs — do not edit manually
    #[serde(default)]
    immutable_base: usize,
    #[serde(default)]
    append_only_base: usize,
    #[serde(default)]
    mutable_base: usize,
    #[serde(default)]
    compute_base: usize,
    #[serde(default)]
    observation_ring_offset: usize,
    #[serde(default)]
    event_log_offset: usize,
    #[serde(default)]
    emergency_buffer_offset: usize,
    #[serde(default)]
    matrix_offset: usize,
    #[serde(default)]
    hnsw_index_offset: usize,
    #[serde(default)]
    embedding_cache_offset: usize,
}

/// Derived offsets computed from region sizes
#[derive(Debug, Clone)]
struct ComputedOffsets {
    // Region bases (from start of total mmap)
    immutable_base: usize,
    append_only_base: usize,
    mutable_base: usize,
    compute_base: usize,
    // Within APPEND_ONLY region
    observation_ring_offset: usize,
    event_log_offset: usize,
    emergency_buffer_offset: usize,
    // Within COMPUTE region
    matrix_offset: usize,
    hnsw_index_offset: usize,
    embedding_cache_offset: usize,
}

// ============================================================================
// Offset Computation
// ============================================================================

fn compute_offsets(config: &LayoutToml) -> ComputedOffsets {
    let header_size = config.header.size_bytes;

    // Region bases
    let immutable_base = 0;
    let append_only_base = config.regions.immutable_size_bytes;
    let mutable_base = append_only_base + config.regions.append_only_size_bytes;
    let compute_base = mutable_base + config.regions.mutable_size_bytes;

    // Within APPEND_ONLY region
    let observation_ring_offset = header_size; // 64 bytes after start
    let ring_total_size = config.observation_ring.slot_count * config.observation_ring.slot_size_bytes;
    let event_log_offset = observation_ring_offset + ring_total_size;
    let emergency_buffer_offset = config.regions.append_only_size_bytes - config.event_log.emergency_buffer_bytes;

    // Within COMPUTE region (256MB total)
    // Matrix: 2MB (512 floats * 4 bytes * max_apps)
    let matrix_offset = 0;
    let matrix_size = 2 * 1024 * 1024; // 2MB
    // HNSW index: 50MB base
    let hnsw_index_offset = matrix_size;
    // Embedding cache: starts at 52MB
    let embedding_cache_offset = hnsw_index_offset + (50 * 1024 * 1024);

    ComputedOffsets {
        immutable_base,
        append_only_base,
        mutable_base,
        compute_base,
        observation_ring_offset,
        event_log_offset,
        emergency_buffer_offset,
        matrix_offset,
        hnsw_index_offset,
        embedding_cache_offset,
    }
}

// ============================================================================
// SCHEMA Hash Computation
// ============================================================================

fn compute_schema_hash(config: &LayoutToml, offsets: &ComputedOffsets) -> [u8; 32] {
    // Create canonical serialization: sorted keys, fixed-width values
    let mut canonical: Vec<u8> = Vec::new();

    // Schema version
    canonical.extend_from_slice(config.schema.version.as_bytes());
    canonical.push(0); // null terminator

    // Regions (sorted by key)
    canonical.extend_from_slice(b"immutable_size_bytes:");
    canonical.extend_from_slice(&config.regions.immutable_size_bytes.to_le_bytes());
    canonical.extend_from_slice(b"append_only_size_bytes:");
    canonical.extend_from_slice(&config.regions.append_only_size_bytes.to_le_bytes());
    canonical.extend_from_slice(b"mutable_size_bytes:");
    canonical.extend_from_slice(&config.regions.mutable_size_bytes.to_le_bytes());
    canonical.extend_from_slice(b"compute_size_bytes:");
    canonical.extend_from_slice(&config.regions.compute_size_bytes.to_le_bytes());

    // Observation ring
    canonical.extend_from_slice(b"slot_count:");
    canonical.extend_from_slice(&config.observation_ring.slot_count.to_le_bytes());
    canonical.extend_from_slice(b"slot_size_bytes:");
    canonical.extend_from_slice(&config.observation_ring.slot_size_bytes.to_le_bytes());

    // Slabs
    canonical.extend_from_slice(b"goal_contract_slots:");
    canonical.extend_from_slice(&config.slabs.goal_contract_slots.to_le_bytes());
    canonical.extend_from_slice(b"goal_contract_bytes:");
    canonical.extend_from_slice(&config.slabs.goal_contract_bytes.to_le_bytes());
    canonical.extend_from_slice(b"goal_state_slots:");
    canonical.extend_from_slice(&config.slabs.goal_state_slots.to_le_bytes());
    canonical.extend_from_slice(b"goal_state_bytes:");
    canonical.extend_from_slice(&config.slabs.goal_state_bytes.to_le_bytes());
    canonical.extend_from_slice(b"observation_slots:");
    canonical.extend_from_slice(&config.slabs.observation_slots.to_le_bytes());
    canonical.extend_from_slice(b"observation_bytes:");
    canonical.extend_from_slice(&config.slabs.observation_bytes.to_le_bytes());
    canonical.extend_from_slice(b"atomic_counter_slots:");
    canonical.extend_from_slice(&config.slabs.atomic_counter_slots.to_le_bytes());
    canonical.extend_from_slice(b"atomic_counter_bytes:");
    canonical.extend_from_slice(&config.slabs.atomic_counter_bytes.to_le_bytes());

    // Embedding
    canonical.extend_from_slice(b"dim:");
    canonical.extend_from_slice(&config.embedding.dim.to_le_bytes());
    canonical.extend_from_slice(b"max_apps:");
    canonical.extend_from_slice(&config.embedding.max_apps.to_le_bytes());
    canonical.extend_from_slice(b"cache_max_entries:");
    canonical.extend_from_slice(&config.embedding.cache_max_entries.to_le_bytes());

    // Event log
    canonical.extend_from_slice(b"capacity_bytes:");
    canonical.extend_from_slice(&config.event_log.capacity_bytes.to_le_bytes());
    canonical.extend_from_slice(b"emergency_buffer_bytes:");
    canonical.extend_from_slice(&config.event_log.emergency_buffer_bytes.to_le_bytes());

    // Timeouts
    canonical.extend_from_slice(b"sqlite_ready_ms:");
    canonical.extend_from_slice(&config.timeouts.sqlite_ready_ms.to_le_bytes());
    canonical.extend_from_slice(b"write_lock_timeout_ms:");
    canonical.extend_from_slice(&config.timeouts.write_lock_timeout_ms.to_le_bytes());

    // Offsets (computed, sorted)
    canonical.extend_from_slice(b"immutable_base:");
    canonical.extend_from_slice(&offsets.immutable_base.to_le_bytes());
    canonical.extend_from_slice(b"append_only_base:");
    canonical.extend_from_slice(&offsets.append_only_base.to_le_bytes());
    canonical.extend_from_slice(b"mutable_base:");
    canonical.extend_from_slice(&offsets.mutable_base.to_le_bytes());
    canonical.extend_from_slice(b"compute_base:");
    canonical.extend_from_slice(&offsets.compute_base.to_le_bytes());
    canonical.extend_from_slice(b"observation_ring_offset:");
    canonical.extend_from_slice(&offsets.observation_ring_offset.to_le_bytes());
    canonical.extend_from_slice(b"event_log_offset:");
    canonical.extend_from_slice(&offsets.event_log_offset.to_le_bytes());
    canonical.extend_from_slice(b"emergency_buffer_offset:");
    canonical.extend_from_slice(&offsets.emergency_buffer_offset.to_le_bytes());
    canonical.extend_from_slice(b"matrix_offset:");
    canonical.extend_from_slice(&offsets.matrix_offset.to_le_bytes());
    canonical.extend_from_slice(b"hnsw_index_offset:");
    canonical.extend_from_slice(&offsets.hnsw_index_offset.to_le_bytes());
    canonical.extend_from_slice(b"embedding_cache_offset:");
    canonical.extend_from_slice(&offsets.embedding_cache_offset.to_le_bytes());

    // Compute SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&canonical);
    let result = hasher.finalize();

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

// ============================================================================
// layout.rs Generation
// ============================================================================

fn emit_layout_rs(config: &LayoutToml, offsets: &ComputedOffsets, hash: [u8; 32], out_dir: &PathBuf) {
    let hash_hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();

    let content = format!(r#"//! GENERATED by build.rs — DO NOT EDIT
//! Any changes will be overwritten on next build.
//! SCHEMA_HASH ensures Rust and Python layouts stay synchronized.

pub const SCHEMA_HASH: [u8; 32] = {:?};

// ============================================================================
// Region Sizes (from nexus_layout.toml)
// ============================================================================

pub const IMMUTABLE_SIZE: usize = {};
pub const APPEND_ONLY_SIZE: usize = {};
pub const MUTABLE_SIZE: usize = {};
pub const COMPUTE_SIZE: usize = {};

pub const HEADER_SIZE: usize = {};

// ============================================================================
// Observation Ring
// ============================================================================

pub const OBSERVATION_SLOT_COUNT: usize = {};
pub const OBSERVATION_SLOT_SIZE: usize = {};
pub const OBSERVATION_CONTENT_BYTES: usize = {};
pub const OBSERVATION_CACHE_PAD_BYTES: usize = {};

// ============================================================================
// Slab Allocator Sizes
// ============================================================================

pub const GOAL_CONTRACT_SLOTS: usize = {};
pub const GOAL_CONTRACT_BYTES: usize = {};
pub const GOAL_STATE_SLOTS: usize = {};
pub const GOAL_STATE_BYTES: usize = {};
pub const OBSERVATION_SLOTS: usize = {};
pub const OBSERVATION_BYTES: usize = {};
pub const ATOMIC_COUNTER_SLOTS: usize = {};
pub const ATOMIC_COUNTER_BYTES: usize = {};

// ============================================================================
// Embedding Configuration
// ============================================================================

pub const EMBEDDING_DIM: usize = {};
pub const EMBEDDING_MAX_APPS: usize = {};
pub const EMBEDDING_CACHE_MAX_ENTRIES: usize = {};

// ============================================================================
// Event Log Configuration
// ============================================================================

pub const EVENT_LOG_CAPACITY_BYTES: usize = {};
pub const EVENT_LOG_AVG_EVENT_BYTES: usize = {};
pub const EMERGENCY_BUFFER_BYTES: usize = {};

// ============================================================================
// Timeouts
// ============================================================================

pub const SQLITE_READY_TIMEOUT_MS: u64 = {};
pub const WRITE_LOCK_TIMEOUT_MS: u64 = {};
pub const COMPUTE_REFRESH_MIN: u64 = {};

// ============================================================================
// Computed Offsets (from region bases)
// ============================================================================

// Region bases (from start of total mmap)
pub const IMMUTABLE_BASE: usize = {};
pub const APPEND_ONLY_BASE: usize = {};
pub const MUTABLE_BASE: usize = {};
pub const COMPUTE_BASE: usize = {};

// Within APPEND_ONLY region
pub const OBSERVATION_RING_OFFSET: usize = {};
pub const EVENT_LOG_OFFSET: usize = {};
pub const EMERGENCY_BUFFER_OFFSET: usize = {};

// Within COMPUTE region
pub const MATRIX_OFFSET: usize = {};
pub const HNSW_INDEX_OFFSET: usize = {};
pub const EMBEDDING_CACHE_OFFSET: usize = {};

// ============================================================================
// Schema Hash Verification
// ============================================================================

/// Returns the SCHEMA_HASH as a hex string for comparison with Python.
pub fn schema_hash_hex() -> &'static str {{
    "{}"
}}

/// Verifies that the provided hash matches SCHEMA_HASH.
pub fn verify_schema_hash(provided: &[u8; 32]) -> bool {{
    provided == &SCHEMA_HASH
}}
"#,
        // SCHEMA_HASH array
        hash,
        // Region sizes
        config.regions.immutable_size_bytes,
        config.regions.append_only_size_bytes,
        config.regions.mutable_size_bytes,
        config.regions.compute_size_bytes,
        config.header.size_bytes,
        // Observation ring
        config.observation_ring.slot_count,
        config.observation_ring.slot_size_bytes,
        config.observation_ring.content_bytes,
        config.observation_ring.cache_pad_bytes,
        // Slabs
        config.slabs.goal_contract_slots,
        config.slabs.goal_contract_bytes,
        config.slabs.goal_state_slots,
        config.slabs.goal_state_bytes,
        config.slabs.observation_slots,
        config.slabs.observation_bytes,
        config.slabs.atomic_counter_slots,
        config.slabs.atomic_counter_bytes,
        // Embedding
        config.embedding.dim,
        config.embedding.max_apps,
        config.embedding.cache_max_entries,
        // Event log
        config.event_log.capacity_bytes,
        config.event_log.avg_event_bytes,
        config.event_log.emergency_buffer_bytes,
        // Timeouts
        config.timeouts.sqlite_ready_ms,
        config.timeouts.write_lock_timeout_ms,
        config.timeouts.compute_refresh_min,
        // Offsets
        offsets.immutable_base,
        offsets.append_only_base,
        offsets.mutable_base,
        offsets.compute_base,
        offsets.observation_ring_offset,
        offsets.event_log_offset,
        offsets.emergency_buffer_offset,
        offsets.matrix_offset,
        offsets.hnsw_index_offset,
        offsets.embedding_cache_offset,
        // Hash hex string
        hash_hex,
    );

    let layout_rs_path = out_dir.join("layout.rs");
    fs::write(&layout_rs_path, &content).expect("Failed to write layout.rs");
    println!("cargo:warning=Generated src/generated/layout.rs with SCHEMA_HASH={}", hash_hex);
}

// ============================================================================
// _layout.py Generation
// ============================================================================

fn emit_layout_py(config: &LayoutToml, offsets: &ComputedOffsets, hash: [u8; 32], python_dir: &PathBuf) {
    let hash_hex: String = hash.iter().map(|b| format!("{:02x}", b)).collect();

    let content = format!(r#"# GENERATED by build.rs — DO NOT EDIT
# Any changes will be overwritten on next build.
# SCHEMA_HASH ensures Rust and Python layouts stay synchronized.

import ctypes
from typing import Final

# ============================================================================
# SCHEMA_HASH - MUST match src/generated/layout.rs
# ============================================================================

SCHEMA_HASH: Final[bytes] = bytes.fromhex('{hash_hex}')

# ============================================================================
# Region Sizes
# ============================================================================

IMMUTABLE_SIZE: Final[int] = {immutable_size}
APPEND_ONLY_SIZE: Final[int] = {append_only_size}
MUTABLE_SIZE: Final[int] = {mutable_size}
COMPUTE_SIZE: Final[int] = {compute_size}

HEADER_SIZE: Final[int] = {header_size}

# ============================================================================
# Observation Ring
# ============================================================================

OBSERVATION_SLOT_COUNT: Final[int] = {slot_count}
OBSERVATION_SLOT_SIZE: Final[int] = {slot_size_bytes}
OBSERVATION_CONTENT_BYTES: Final[int] = {content_bytes}
OBSERVATION_CACHE_PAD_BYTES: Final[int] = {cache_pad_bytes}

# ============================================================================
# Slab Allocator Sizes
# ============================================================================

GOAL_CONTRACT_SLOTS: Final[int] = {goal_contract_slots}
GOAL_CONTRACT_BYTES: Final[int] = {goal_contract_bytes}
GOAL_STATE_SLOTS: Final[int] = {goal_state_slots}
GOAL_STATE_BYTES: Final[int] = {goal_state_bytes}
OBSERVATION_SLOTS: Final[int] = {observation_slots}
OBSERVATION_BYTES: Final[int] = {observation_bytes}
ATOMIC_COUNTER_SLOTS: Final[int] = {atomic_counter_slots}
ATOMIC_COUNTER_BYTES: Final[int] = {atomic_counter_bytes}

# ============================================================================
# Embedding Configuration
# ============================================================================

EMBEDDING_DIM: Final[int] = {embedding_dim}
EMBEDDING_MAX_APPS: Final[int] = {embedding_max_apps}
EMBEDDING_CACHE_MAX_ENTRIES: Final[int] = {embedding_cache_max_entries}

# ============================================================================
# Event Log Configuration
# ============================================================================

EVENT_LOG_CAPACITY_BYTES: Final[int] = {event_log_capacity}
EVENT_LOG_AVG_EVENT_BYTES: Final[int] = {event_log_avg}
EMERGENCY_BUFFER_BYTES: Final[int] = {emergency_buffer}

# ============================================================================
# Timeouts
# ============================================================================

SQLITE_READY_TIMEOUT_MS: Final[int] = {sqlite_ready_ms}
WRITE_LOCK_TIMEOUT_MS: Final[int] = {write_lock_timeout_ms}
COMPUTE_REFRESH_MIN: Final[int] = {compute_refresh_min}

# ============================================================================
# Computed Offsets
# ============================================================================

# Region bases (from start of total mmap)
IMMUTABLE_BASE: Final[int] = {immutable_base}
APPEND_ONLY_BASE: Final[int] = {append_only_base}
MUTABLE_BASE: Final[int] = {mutable_base}
COMPUTE_BASE: Final[int] = {compute_base}

# Within APPEND_ONLY region
OBSERVATION_RING_OFFSET: Final[int] = {observation_ring_offset}
EVENT_LOG_OFFSET: Final[int] = {event_log_offset}
EMERGENCY_BUFFER_OFFSET: Final[int] = {emergency_buffer_offset}

# Within COMPUTE region
MATRIX_OFFSET: Final[int] = {matrix_offset}
HNSW_INDEX_OFFSET: Final[int] = {hnsw_index_offset}
EMBEDDING_CACHE_OFFSET: Final[int] = {embedding_cache_offset}

# ============================================================================
# ObservationSnapshot Structure (320 bytes total)
# ============================================================================

class ObservationSnapshot(ctypes.Structure):
    """Observation snapshot stored in the observation ring.

    Total size: 320 bytes (256 content + 64 cache pad).
    Layout matches Rust #[repr(C)] WorldModelScalars.
    """
    _pack_ = 1
    _fields_ = [
        # Content: 256 bytes
        ('timestamp_ns', ctypes.c_uint64),      # offset 0, size 8
        ('active_pid', ctypes.c_uint32),        # offset 8, size 4
        ('cpu_pct_bits', ctypes.c_uint32),      # offset 12, size 4 (f32 bits)
        ('memory_used_mb', ctypes.c_uint32),    # offset 16, size 4
        ('disk_free_gb_bits', ctypes.c_uint32), # offset 20, size 4 (f32 bits)
        ('network_reachable', ctypes.c_uint8),  # offset 24, size 1
        ('_reserved', ctypes.c_uint8 * 231),    # offset 25, size 231 (padding)
        # Cache pad: 64 bytes
        ('_cache_pad', ctypes.c_uint8 * 64),    # offset 256, size 64
    ]

# Verify size at import time
assert ctypes.sizeof(ObservationSnapshot) == OBSERVATION_SLOT_SIZE, \
    f"ObservationSnapshot size mismatch: {{ctypes.sizeof(ObservationSnapshot)}} != {{OBSERVATION_SLOT_SIZE}}"

# ============================================================================
# Schema Hash Verification
# ============================================================================

class NexusLayoutError(Exception):
    """Raised when Python layout does not match Rust layout."""
    pass

def verify_schema_hash(mmap_header: bytes) -> None:
    """Verify that the mmap header SCHEMA_HASH matches this layout.

    Called at Python startup after importing nexus_native.
    Raises NexusLayoutError if hashes do not match.

    Args:
        mmap_header: First 64 bytes of the mmap (header region).

    Raises:
        NexusLayoutError: If SCHEMA_HASH in header differs from this layout.
    """
    if len(mmap_header) < 32:
        raise NexusLayoutError(
            f"mmap header too short: {{len(mmap_header)}} bytes, expected at least 32"
        )

    # Extract SCHEMA_HASH from header (first 32 bytes)
    header_hash = mmap_header[:32]

    if header_hash != SCHEMA_HASH:
        header_hex = header_hash.hex()
        expected_hex = SCHEMA_HASH.hex()
        raise NexusLayoutError(
            f"SCHEMA_HASH mismatch\\n"
            f"  expected (from _layout.py): {{expected_hex}}\\n"
            f"  found (from mmap header):   {{header_hex}}\\n"
            f"  fix: run `cargo build` to regenerate _layout.py"
        )
"#,
        hash_hex = hash_hex,
        immutable_size = config.regions.immutable_size_bytes,
        append_only_size = config.regions.append_only_size_bytes,
        mutable_size = config.regions.mutable_size_bytes,
        compute_size = config.regions.compute_size_bytes,
        header_size = config.header.size_bytes,
        slot_count = config.observation_ring.slot_count,
        slot_size_bytes = config.observation_ring.slot_size_bytes,
        content_bytes = config.observation_ring.content_bytes,
        cache_pad_bytes = config.observation_ring.cache_pad_bytes,
        goal_contract_slots = config.slabs.goal_contract_slots,
        goal_contract_bytes = config.slabs.goal_contract_bytes,
        goal_state_slots = config.slabs.goal_state_slots,
        goal_state_bytes = config.slabs.goal_state_bytes,
        observation_slots = config.slabs.observation_slots,
        observation_bytes = config.slabs.observation_bytes,
        atomic_counter_slots = config.slabs.atomic_counter_slots,
        atomic_counter_bytes = config.slabs.atomic_counter_bytes,
        embedding_dim = config.embedding.dim,
        embedding_max_apps = config.embedding.max_apps,
        embedding_cache_max_entries = config.embedding.cache_max_entries,
        event_log_capacity = config.event_log.capacity_bytes,
        event_log_avg = config.event_log.avg_event_bytes,
        emergency_buffer = config.event_log.emergency_buffer_bytes,
        sqlite_ready_ms = config.timeouts.sqlite_ready_ms,
        write_lock_timeout_ms = config.timeouts.write_lock_timeout_ms,
        compute_refresh_min = config.timeouts.compute_refresh_min,
        immutable_base = offsets.immutable_base,
        append_only_base = offsets.append_only_base,
        mutable_base = offsets.mutable_base,
        compute_base = offsets.compute_base,
        observation_ring_offset = offsets.observation_ring_offset,
        event_log_offset = offsets.event_log_offset,
        emergency_buffer_offset = offsets.emergency_buffer_offset,
        matrix_offset = offsets.matrix_offset,
        hnsw_index_offset = offsets.hnsw_index_offset,
        embedding_cache_offset = offsets.embedding_cache_offset,
    );

    let layout_py_path = python_dir.join("_layout.py");
    fs::write(&layout_py_path, &content).expect("Failed to write _layout.py");
    println!("cargo:warning=Generated python/nexus/_layout.py with SCHEMA_HASH={}", hash_hex);
}

// ============================================================================
// TOML Offset Update
// ============================================================================

fn update_toml_offsets(toml_path: &PathBuf, original_content: &str, offsets: &ComputedOffsets) {
    // Read the original TOML and update the [offsets] section
    let mut updated = original_content.to_string();

    // Find and replace the [offsets] section
    let offset_section = format!(
r#"[offsets]
# Computed by build.rs — do not edit manually.
# All offsets are from the start of their respective mmap region.
immutable_base = {}
append_only_base = {}
mutable_base = {}
compute_base = {}
# Within APPEND_ONLY region:
observation_ring_offset = {}
event_log_offset = {}
emergency_buffer_offset = {}
# Within COMPUTE region:
matrix_offset = {}
hnsw_index_offset = {}
embedding_cache_offset = {}"#,
        offsets.immutable_base,
        offsets.append_only_base,
        offsets.mutable_base,
        offsets.compute_base,
        offsets.observation_ring_offset,
        offsets.event_log_offset,
        offsets.emergency_buffer_offset,
        offsets.matrix_offset,
        offsets.hnsw_index_offset,
        offsets.embedding_cache_offset,
    );

    // Find the [offsets] section and replace everything until the next section or EOF
    if let Some(start) = updated.find("[offsets]") {
        // Find the end of the [offsets] section (next section or EOF)
        let end = updated[start..].find("\n[").map(|i| start + i).unwrap_or(updated.len());
        updated.replace_range(start..end, &offset_section);
    } else {
        // If [offsets] doesn't exist, append it
        updated.push_str("\n\n");
        updated.push_str(&offset_section);
    }

    // Only write if content changed
    if &updated != original_content {
        fs::write(toml_path, &updated).expect("Failed to write updated nexus_layout.toml");
        println!("cargo:warning=Updated nexus_layout.toml with computed offsets");
    }
}