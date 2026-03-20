//! agent_registry.rs — AgentRegistryEntry with heartbeat AtomicU64 and status.
//!
//! PURPOSE (Layer 4, file #15 from plan):
//! Agent registry entries — one per active agent (max 64).
//! 512 bytes per slot. heartbeat AtomicU64, status AtomicU8.
//! Written by each agent on heartbeat. Read by L1 Health Watchdog.

use std::sync::atomic::AtomicU64;

/// Agent registry entry (512 bytes per slot).
/// heartbeat_ts is AtomicU64 — updated by the agent every 200ms.
/// Kept separate from the RwLock fields to allow lockless heartbeat updates.
#[repr(C)]
pub struct AgentRegistryEntry {
    pub agent_id: u64,        // offset 0, 8 bytes
    pub agent_type: u8,       // offset 8, 1 byte
    pub status: u8,           // offset 9, 1 byte — 0=IDLE, 1=RUNNING, 2=STALLED, 3=TERMINATED
    // 2 bytes implicit padding for u32 alignment
    pub failure_count: u32,   // offset 12, 4 bytes
    pub heartbeat_ts: AtomicU64,  // offset 16, 8 bytes
    pub current_goal_id: u64, // offset 24, 8 bytes
    pub tokens_consumed: i64, // offset 32, 8 bytes
    pub _pad: [u8; 472],      // offset 40, fills to 512 bytes
}

const _: () = assert!(
    std::mem::size_of::<AgentRegistryEntry>() == 512,
    "AgentRegistryEntry must be exactly 512 bytes"
);

impl Default for AgentRegistryEntry {
    fn default() -> Self {
        Self {
            agent_id: 0,
            agent_type: 0,
            status: 0,
            failure_count: 0,
            heartbeat_ts: AtomicU64::new(0),
            current_goal_id: 0,
            tokens_consumed: 0,
            _pad: [0; 472],
        }
    }
}
