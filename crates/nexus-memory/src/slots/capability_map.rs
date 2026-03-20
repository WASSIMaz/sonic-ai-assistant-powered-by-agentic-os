//! capability_map.rs — task_sig → [resource_id] hot-reloadable routing.
//!
//! PURPOSE (Layer 4, file #18 from plan):
//! CapabilityMapEntry — task_sig to resource_id routing table. 1024 bytes per slot.
//! Hot-reloadable: hot_reload_version increments on each live reload.

/// Capability map routing entry (1024 bytes per slot).
/// Layout:
///   task_sig [u8; 64]    → offset 0, 64 bytes
///   resource_ids [u32;16]→ offset 64, 64 bytes
///   resource_count (u8)  → offset 128, 1 byte
///   3 bytes implicit padding for u32 alignment
///   hot_reload_version(u32) → offset 132, 4 bytes
///   _pad                 → offset 136, 888 bytes → total 1024
#[repr(C)]
pub struct CapabilityMapEntry {
    pub task_sig: [u8; 64],
    pub resource_ids: [u32; 16],
    pub resource_count: u8,
    pub hot_reload_version: u32,
    pub _pad: [u8; 888],
}

const _: () = assert!(
    std::mem::size_of::<CapabilityMapEntry>() == 1024,
    "CapabilityMapEntry must be exactly 1024 bytes"
);

impl Default for CapabilityMapEntry {
    fn default() -> Self {
        Self {
            task_sig: [0; 64],
            resource_ids: [0; 16],
            resource_count: 0,
            hot_reload_version: 0,
            _pad: [0; 888],
        }
    }
}
