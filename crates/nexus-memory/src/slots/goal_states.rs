//! goal_states.rs — GoalStateEntry for GoalDependencyGraph (Slab B).
//!
//! PURPOSE (Layer 4, file #14 from plan):
//! GoalState/SubGoalNode slots. Slab B allocates these.
//! 1024 bytes per slot. Maps a sub-goal ID to its dependency graph node.
//! Written by the Kernel when accepting a goal contract.
//! Read by the Contextualizer and ProcessOrchestrator.

/// GoalState entry stored in Slab B (1024 bytes per slot).
/// Maps a sub-goal ID to its dependency graph node.
/// Layout:
///   goal_id (u64)           → offset 0, 8 bytes
///   parent_goal_id (u64)    → offset 8, 8 bytes
///   status (u8)             → offset 16, 1 byte
///   agent_type (u8)         → offset 17, 1 byte
///   critical_path_len (u16) → offset 18, 2 bytes
///   4 bytes implicit padding for [u64;8] alignment
///   depends_on [u64; 8]     → offset 24, 64 bytes
///   contract_ref (u32)      → offset 88, 4 bytes
///   result_payload_len (u32)→ offset 92, 4 bytes
///   result_payload          → offset 96, 928 bytes → total 1024
#[repr(C)]
pub struct GoalStateEntry {
    pub goal_id: u64,
    pub parent_goal_id: u64,
    pub status: u8,           // 0=PENDING, 1=RUNNING, 2=COMPLETE, 3=FAILED
    pub agent_type: u8,
    pub critical_path_len: u16,
    pub depends_on: [u64; 8], // up to 8 dependency goal IDs
    pub contract_ref: u32,    // index into GoalContract slab
    pub result_payload_len: u32,
    pub result_payload: [u8; 928], // inline payload up to 928 bytes
}

const _: () = assert!(
    std::mem::size_of::<GoalStateEntry>() == 1024,
    "GoalStateEntry must be exactly 1024 bytes"
);

impl Default for GoalStateEntry {
    fn default() -> Self {
        Self {
            goal_id: 0,
            parent_goal_id: 0,
            status: 0,
            agent_type: 0,
            critical_path_len: 0,
            depends_on: [0; 8],
            contract_ref: 0,
            result_payload_len: 0,
            result_payload: [0; 928],
        }
    }
}
