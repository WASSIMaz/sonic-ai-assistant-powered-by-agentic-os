//! error.rs — All error types for the nexus-memory crate.
//!
//! Every error type implements std::error::Error and is Send + Sync.
//! Error variants map to the failure modes defined in the architecture:
//! - MmapError: OS-level memory mapping failures
//! - SlabExhausted: all slots in a slab class are allocated
//! - LockTimeout: write lock not acquired within 500ms
//! - NamespaceViolation: agent attempted access outside its frozenset
//! - BootError: fatal failure during NexusMemory::init()
//! - SchemaHashMismatch: layout.rs and _layout.py disagree
//! - SqliteNotReady: event written before sqlite_ready was set
//! - BufferError: event_log_buffer append failed

use std::borrow::Cow;
use nexus_utils::error::SlotId;
use nexus_utils::AgentId;
use thiserror::Error;

/// Errors from OS-level memory mapping operations.
/// Wraps platform-specific error codes from mprotect/VirtualProtect/mmap.
#[derive(Debug, Error)]
pub enum MmapError {
    /// mmap/CreateFileMapping call failed.
    #[error("mmap allocation failed: size={size}, os_error={os_error}")]
    AllocationFailed { size: usize, os_error: i32 },

    /// mprotect/VirtualProtect to PROT_READ failed.
    #[error("seal (PROT_READ) failed on region '{name}': os_error={os_error}")]
    SealFailed { name: &'static str, os_error: i32 },

    /// mprotect/VirtualProtect to PROT_READ|PROT_WRITE failed.
    #[error("unseal (PROT_READ|WRITE) failed on region '{name}': os_error={os_error}")]
    UnsealFailed { name: &'static str, os_error: i32 },

    /// munmap/UnmapViewOfFile failed.
    #[error("munmap failed on region '{name}': os_error={os_error}")]
    UnmapFailed { name: &'static str, os_error: i32 },
}

/// Slab allocator exhaustion — all slots in a slab class are in use.
/// The goal acceptance gate must refuse the goal and publish RESOURCE_EXHAUSTED
/// to the Bus before the slab allocator is called.
#[derive(Debug, Error)]
#[error("slab exhausted: slab_class='{slab_class}', capacity={capacity}")]
pub struct SlabExhausted {
    pub slab_class: &'static str,
    pub capacity: usize,
}

/// parking_lot write lock not acquired within the 500ms timeout.
/// The caller must publish LOCK_TIMEOUT to the Bus with slot_id and holder_agent_id.
#[derive(Debug, Error)]
#[error("lock timeout: slot_id={slot_id}, holder_agent_id={holder_agent_id:?}, waited_ms={waited_ms}")]
pub struct LockTimeoutError {
    pub slot_id: SlotId,
    pub holder_agent_id: Option<AgentId>,
    pub waited_ms: u64,
}

/// Agent attempted access to a TypedBuffer slot outside its CapabilityToken namespace.
/// Triggers ACCESS event log write (non-blocking) and returns NamespaceViolationSentinel.
#[derive(Debug, Error)]
#[error("namespace violation: agent={agent_id}, slot={slot_id}, op={operation}, lamport={lamport_ts}")]
pub struct NamespaceViolationError {
    pub slot_id: SlotId,
    pub agent_id: AgentId,
    pub lamport_ts: u64,
    pub operation: Cow<'static, str>,
}

/// Fatal boot failure. NEXUS refuses to start.
/// Raised when SQLite fails to initialize within the 5-second timeout,
/// or when the schema hash in the mmap header does not match layout.rs.
#[derive(Debug, Error)]
pub enum BootError {
    /// SQLite failed to initialize within sqlite_ready_ms.
    #[error("SQLite init failed after {elapsed_ms}ms (timeout={timeout_ms}ms): {detail}")]
    SqliteInitFailed {
        elapsed_ms: u64,
        timeout_ms: u64,
        detail: String,
    },

    /// Schema hash mismatch between layout.rs (Rust binary) and _layout.py.
    #[error("schema hash mismatch: binary_hash={binary_hash}, layout_py_hash={layout_py_hash}")]
    SchemaHashMismatch {
        binary_hash: String,
        layout_py_hash: String,
    },

    /// mmap allocation failed during region initialization.
    #[error("mmap init failed: {0}")]
    MmapFailed(#[from] MmapError),

    /// An unexpected OS error prevented boot completion.
    #[error("os error during boot: {detail}")]
    OsError { detail: String },
}

/// Event log buffer append failed.
#[derive(Debug, Error)]
pub enum BufferError {
    /// Main buffer is full and emergency buffer is also full.
    #[error("event_log_buffer full: capacity={capacity}")]
    Full { capacity: usize },

    /// Serialization of EventRecord failed.
    #[error("event serialization failed: {detail}")]
    SerializationFailed { detail: String },
}