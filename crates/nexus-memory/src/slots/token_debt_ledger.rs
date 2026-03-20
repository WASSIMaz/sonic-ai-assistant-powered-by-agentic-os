//! token_debt_ledger.rs — cumulative_debt, call_count, avg_overrun per agent.
//!
//! PURPOSE (Layer 4, file #19 from plan):
//! TokenDebtEntry — per-agent token debt for IST budget enforcement.
//! 256 bytes per slot. Written by the TokenDebt subsystem after each LLM call.

/// Token debt ledger entry per agent (256 bytes per slot).
/// Layout:
///   agent_id (u64)            → offset 0, 8 bytes
///   cumulative_debt (i64)     → offset 8, 8 bytes
///   call_count (u64)          → offset 16, 8 bytes
///   avg_overrun_tokens (f32)  → offset 24, 4 bytes
///   4 bytes implicit padding for u64 alignment
///   last_overrun_lamport (u64)→ offset 32, 8 bytes
///   _pad                      → offset 40, 216 bytes → total 256
#[repr(C)]
pub struct TokenDebtEntry {
    pub agent_id: u64,
    pub cumulative_debt: i64,
    pub call_count: u64,
    pub avg_overrun_tokens: f32,
    pub last_overrun_lamport: u64,
    pub _pad: [u8; 216],
}

const _: () = assert!(
    std::mem::size_of::<TokenDebtEntry>() == 256,
    "TokenDebtEntry must be exactly 256 bytes"
);

impl Default for TokenDebtEntry {
    fn default() -> Self {
        Self {
            agent_id: 0,
            cumulative_debt: 0,
            call_count: 0,
            avg_overrun_tokens: 0.0,
            last_overrun_lamport: 0,
            _pad: [0; 216],
        }
    }
}
