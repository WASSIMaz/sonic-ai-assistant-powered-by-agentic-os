//! surface_assignments.rs — surface_id → agent_id mapping.
//!
//! PURPOSE (Layer 4, file #17 from plan):
//! SurfaceAssignmentEntry — surface_id to agent_id mapping. 64 bytes per slot.
//! Written by the IST subsystem when an agent acquires a display surface.

/// Surface-to-agent assignment entry (64 bytes per slot).
/// Layout:
///   surface_id (u32)      → offset 0, 4 bytes
///   4 bytes implicit padding for u64 alignment
///   agent_id (u64)        → offset 8, 8 bytes
///   assigned_lamport (u64)→ offset 16, 8 bytes
///   _pad                  → offset 24, 40 bytes → total 64
#[repr(C)]
pub struct SurfaceAssignmentEntry {
    pub surface_id: u32,
    pub agent_id: u64,
    pub assigned_lamport: u64,
    pub _pad: [u8; 40],
}

const _: () = assert!(
    std::mem::size_of::<SurfaceAssignmentEntry>() == 64,
    "SurfaceAssignmentEntry must be exactly 64 bytes"
);

impl Default for SurfaceAssignmentEntry {
    fn default() -> Self {
        Self {
            surface_id: 0,
            agent_id: 0,
            assigned_lamport: 0,
            _pad: [0; 40],
        }
    }
}
