//! context_envelope.rs — ContextEnvelopeEntry for the MUTABLE region.
//!
//! PURPOSE:
//! Each running specialist agent gets a ContextEnvelope — a managed container
//! for everything in its current reasoning context — stored as a MUTABLE_REGION
//! entry alongside the GoalContract.
//!
//! TOKEN ZONE ACCOUNTING:
//! The 6K-token context window is divided into zones tracked by this struct:
//!   Anchor Zone       — max 900 tokens:  immutable high-value facts
//!   Working Zone      — variable:         current sub-task context
//!   Recent History    — max 3000 tokens:  last N steps of reasoning
//!   Compressed History— max 1500 tokens:  summarized older steps
//!   Semantic Anchors  — max 400 tokens:   retrieved embeddings
//!
//! COMPRESSION:
//! compression_level tracks the current compression state of the envelope:
//!   0 = none        (fresh envelope, below threshold)
//!   1 = Level1 dedup (remove duplicate observations)
//!   2 = Level2 summarize (summarize older steps into compressed_history)
//!   3 = Level3 meta  (meta-summary, only key facts retained)
//!
//! OWNERSHIP:
//! Written by the Contextualizer when enriching a sub-goal with context.
//! Read by the ProcessOrchestrator when dispatching work to an agent.
//! Updated by the agent's hook (Chronicler) after each reasoning step.

/// ContextEnvelopeEntry stored in the MUTABLE region alongside GoalStateEntry.
///
/// Fixed-size 1024-byte slot for slab allocation alignment with GoalStateEntry.
///
/// Layout:
///   goal_id                (u64)       → offset 0,   8 bytes
///   agent_id               (u64)       → offset 8,   8 bytes
///   anchor_token_count     (u32)       → offset 16,  4 bytes  (max 900)
///   working_token_count    (u32)       → offset 20,  4 bytes
///   recent_token_count     (u32)       → offset 24,  4 bytes  (max 3000)
///   compressed_token_count (u32)       → offset 28,  4 bytes  (max 1500)
///   semantic_token_count   (u32)       → offset 32,  4 bytes  (max 400)
///   compression_level      (u8)        → offset 36,  1 byte
///   _pad1                  ([u8; 3])   → offset 37,  3 bytes  (align u32)
///   last_compressed_step   (u32)       → offset 40,  4 bytes
///   total_steps            (u32)       → offset 44,  4 bytes
///   payload                ([u8; 960]) → offset 48, 960 bytes (compressed content)
///   _pad                   ([u8; 16])  → offset 1008, 16 bytes (pad to 1024)
///   Total: 1024 bytes
#[repr(C)]
pub struct ContextEnvelopeEntry {
    /// Which goal this envelope belongs to (FK to GoalStateEntry).
    pub goal_id: u64,

    /// Which agent owns this context envelope.
    pub agent_id: u64,

    /// Current token count in the Anchor Zone (max 900 tokens).
    /// Contains immutable high-value facts anchored at session start.
    pub anchor_token_count: u32,

    /// Current token count in the Working Zone (variable).
    /// Contains the current sub-task context and active reasoning.
    pub working_token_count: u32,

    /// Current token count in the Recent History Zone (max 3000 tokens).
    /// Contains the last N steps of reasoning, FIFO-evicted when full.
    pub recent_token_count: u32,

    /// Current token count in the Compressed History Zone (max 1500 tokens).
    /// Contains summarized older steps, produced by Level2/Level3 compression.
    pub compressed_token_count: u32,

    /// Current token count in the Semantic Anchor Zone (max 400 tokens).
    /// Contains embeddings retrieved from the HNSW index for this context.
    pub semantic_token_count: u32,

    /// Current compression level applied to this envelope:
    ///   0 = none        (below threshold, no compression applied)
    ///   1 = Level1 dedup (duplicate observations removed)
    ///   2 = Level2 summarize (older steps summarized into compressed_history)
    ///   3 = Level3 meta  (meta-summary, only key facts retained)
    pub compression_level: u8,

    /// Alignment padding to 4-byte boundary before next u32 field.
    pub _pad1: [u8; 3],

    /// The reasoning step index at which the last compression was triggered.
    /// Used to avoid compressing again too soon after the previous compression.
    pub last_compressed_step: u32,

    /// Total reasoning steps completed so far in this goal.
    /// Incremented by the Chronicler hook after each step.
    pub total_steps: u32,

    /// Serialized compressed content payload (MessagePack format).
    /// Written by the Contextualizer; contains enriched context for the agent.
    /// If the full content exceeds 960 bytes it is spilled to warm-tier SQLite
    /// and this field holds a 32-byte content-addressed reference hash instead.
    pub payload: [u8; 960],

    /// Padding to reach exactly 1024 bytes.
    pub _pad: [u8; 16],
}

// Compile-time size assertion: must be exactly 1024 bytes for slab alignment.
const _: () = assert!(
    std::mem::size_of::<ContextEnvelopeEntry>() == 1024,
    "ContextEnvelopeEntry must be exactly 1024 bytes"
);

/// Compression level constants for ContextEnvelopeEntry::compression_level.
pub mod compression_level {
    /// No compression applied — envelope is below the compression threshold.
    pub const NONE: u8 = 0;
    /// Level 1: duplicate observation removal only.
    pub const DEDUP: u8 = 1;
    /// Level 2: older steps summarized into compressed history zone.
    pub const SUMMARIZE: u8 = 2;
    /// Level 3: meta-summary, only critical facts retained.
    pub const META: u8 = 3;
}

/// Token zone capacity limits (in tokens).
pub mod token_limits {
    pub const ANCHOR_MAX: u32 = 900;
    pub const RECENT_MAX: u32 = 3000;
    pub const COMPRESSED_MAX: u32 = 1500;
    pub const SEMANTIC_MAX: u32 = 400;
}

impl Default for ContextEnvelopeEntry {
    fn default() -> Self {
        Self {
            goal_id: 0,
            agent_id: 0,
            anchor_token_count: 0,
            working_token_count: 0,
            recent_token_count: 0,
            compressed_token_count: 0,
            semantic_token_count: 0,
            compression_level: compression_level::NONE,
            _pad1: [0; 3],
            last_compressed_step: 0,
            total_steps: 0,
            payload: [0; 960],
            _pad: [0; 16],
        }
    }
}

impl ContextEnvelopeEntry {
    /// Returns the total token count across all zones.
    #[inline]
    pub fn total_tokens(&self) -> u32 {
        self.anchor_token_count
            .saturating_add(self.working_token_count)
            .saturating_add(self.recent_token_count)
            .saturating_add(self.compressed_token_count)
            .saturating_add(self.semantic_token_count)
    }

    /// Returns true if any zone is at or over its capacity limit.
    #[inline]
    pub fn needs_compression(&self) -> bool {
        self.recent_token_count >= token_limits::RECENT_MAX
            || self.compressed_token_count >= token_limits::COMPRESSED_MAX
            || self.anchor_token_count >= token_limits::ANCHOR_MAX
            || self.semantic_token_count >= token_limits::SEMANTIC_MAX
    }

    /// Returns the payload slice up to the used length (first non-zero region).
    /// Callers that know the actual payload length should slice directly.
    pub fn payload_slice(&self) -> &[u8] {
        &self.payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_is_1024_bytes() {
        assert_eq!(std::mem::size_of::<ContextEnvelopeEntry>(), 1024);
    }

    #[test]
    fn test_default_all_zero_tokens() {
        let entry = ContextEnvelopeEntry::default();
        assert_eq!(entry.goal_id, 0);
        assert_eq!(entry.agent_id, 0);
        assert_eq!(entry.anchor_token_count, 0);
        assert_eq!(entry.working_token_count, 0);
        assert_eq!(entry.recent_token_count, 0);
        assert_eq!(entry.compressed_token_count, 0);
        assert_eq!(entry.semantic_token_count, 0);
        assert_eq!(entry.compression_level, compression_level::NONE);
        assert_eq!(entry.total_tokens(), 0);
    }

    #[test]
    fn test_total_tokens_sums_all_zones() {
        let mut entry = ContextEnvelopeEntry::default();
        entry.anchor_token_count = 100;
        entry.working_token_count = 200;
        entry.recent_token_count = 300;
        entry.compressed_token_count = 150;
        entry.semantic_token_count = 50;
        assert_eq!(entry.total_tokens(), 800);
    }

    #[test]
    fn test_needs_compression_when_recent_full() {
        let mut entry = ContextEnvelopeEntry::default();
        assert!(!entry.needs_compression());
        entry.recent_token_count = token_limits::RECENT_MAX;
        assert!(entry.needs_compression());
    }

    #[test]
    fn test_needs_compression_when_anchor_full() {
        let mut entry = ContextEnvelopeEntry::default();
        entry.anchor_token_count = token_limits::ANCHOR_MAX;
        assert!(entry.needs_compression());
    }

    #[test]
    fn test_compression_level_constants() {
        assert_eq!(compression_level::NONE, 0);
        assert_eq!(compression_level::DEDUP, 1);
        assert_eq!(compression_level::SUMMARIZE, 2);
        assert_eq!(compression_level::META, 3);
    }

    #[test]
    fn test_goal_and_agent_assignment() {
        let mut entry = ContextEnvelopeEntry::default();
        entry.goal_id = 42;
        entry.agent_id = 7;
        entry.total_steps = 15;
        entry.last_compressed_step = 10;
        assert_eq!(entry.goal_id, 42);
        assert_eq!(entry.agent_id, 7);
        assert_eq!(entry.total_steps, 15);
        assert_eq!(entry.last_compressed_step, 10);
    }
}
