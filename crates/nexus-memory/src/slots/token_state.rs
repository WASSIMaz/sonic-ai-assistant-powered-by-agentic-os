//! token_state.rs — Input Surface Token atomic CAS acquire/release.
//!
//! PURPOSE (Layer 4, file #16 from plan):
//! TokenStateEntry — atomic CAS acquire/release per surface token.
//! 64 bytes per slot. State machine: FREE(0) → ACQUIRED(1) → FREE(0).

/// Input Surface Token state entry (64 bytes per slot).
/// State machine: FREE(0) → ACQUIRED(1) via atomic CAS in DisplayPool::acquire().
#[repr(C)]
pub struct TokenStateEntry {
    pub surface_id: u32,
    pub state: u8,            // 0=FREE, 1=ACQUIRED
    pub holder_agent_id: u64,
    pub acquired_lamport: u64,
    pub _pad: [u8; 40],
}

const _: () = assert!(
    std::mem::size_of::<TokenStateEntry>() == 64,
    "TokenStateEntry must be exactly 64 bytes"
);

impl Default for TokenStateEntry {
    fn default() -> Self {
        Self {
            surface_id: 0,
            state: 0,
            holder_agent_id: 0,
            acquired_lamport: 0,
            _pad: [0; 40],
        }
    }
}
