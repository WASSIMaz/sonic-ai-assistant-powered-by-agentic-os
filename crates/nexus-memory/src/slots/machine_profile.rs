//! machine_profile.rs — Frozen MachineProfile metadata for the hot-tier mmap slot.
//!
//! PURPOSE (Layer 4, file #20 from plan):
//! Metadata-only struct for the hot-tier mmap representation of MachineProfile.
//! The full profile (app_names, app_capability_descriptions) lives in the Python
//! frozen dataclass held via ArcSwap<Py<PyAny>> in MachineProfileStore.
//! This Rust struct stores ONLY the fields needed by the mmap fast-path:
//!   - live_app_count: used to reshape the capability matrix as (live_app_count, 512)
//!   - captured_at_ns: monotonic timestamp for staleness detection
//!
//! OWNERSHIP:
//! Written by the CPU Worker Pool during the 5-minute capability matrix refresh.
//! Read by Contextualizer threads (via matrix_slice) and L2 Watchdog (staleness).
//! Atomic replacement: ArcSwap::store(Arc::new(new_profile)) in MachineProfileStore.

/// Metadata-only mmap slot for the machine's application profile.
///
/// This struct is the Rust-side fast-path representation. It does NOT contain
/// `app_names` or `app_capability_descriptions` — those live in the Python
/// frozen dataclass (`MachineProfileStore` holds them via `ArcSwap<Py<PyAny>>`).
///
/// The capability matrix (raw float32 mmap bytes) lives in ComputeRegion.
/// This struct provides only the metadata needed to interpret that matrix.
///
/// Layout:
///   live_app_count  (u32)  → offset 0,  4 bytes
///   schema_version  (u32)  → offset 4,  4 bytes
///   captured_at_ns  (u64)  → offset 8,  8 bytes
///   sources_complete (u8)  → offset 16, 1 byte  (1 = all sources finished, 0 = partial)
///   _pad            ([u8;7])→ offset 17, 7 bytes (alignment to 8-byte boundary)
///   Total: 24 bytes
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MachineProfile {
    /// Number of installed applications detected at refresh time.
    /// Used to reshape the capability matrix as (live_app_count, 512).
    /// Written by CPU Worker Pool; read by Contextualizer threads.
    pub live_app_count: u32,

    /// Layout schema version — incremented when the struct definition changes.
    /// Allows the Python side to detect incompatible layout changes at runtime.
    pub schema_version: u32,

    /// The monotonic timestamp (nanoseconds) when this profile was captured.
    /// Used by L2 Watchdog to detect stale profiles (>10 minutes = alert).
    pub captured_at_ns: u64,

    /// True (1) when all data sources (OS app list, hardware probes) have reported.
    /// False (0) during a partial refresh where some sources are still pending.
    /// Contextualizer must not use the matrix until sources_complete == 1.
    pub sources_complete: u8,

    /// Alignment padding to 8-byte boundary.
    pub _pad: [u8; 7],
}

// Compile-time size assertion: 4 + 4 + 8 + 1 + 7 = 24 bytes
const _: () = assert!(
    std::mem::size_of::<MachineProfile>() == 24,
    "MachineProfile must be exactly 24 bytes"
);

impl MachineProfile {
    /// Create an empty profile (used as the initial sentinel before first refresh).
    pub fn empty() -> Self {
        Self {
            live_app_count: 0,
            schema_version: 1,
            captured_at_ns: 0,
            sources_complete: 0,
            _pad: [0; 7],
        }
    }

    /// Returns true if this profile has never been refreshed (sentinel value).
    pub fn is_empty(&self) -> bool {
        self.live_app_count == 0 && self.captured_at_ns == 0
    }

    /// Returns true when all data sources are complete and the matrix is usable.
    pub fn is_complete(&self) -> bool {
        self.sources_complete == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_profile_is_sentinel() {
        let profile = MachineProfile::empty();
        assert_eq!(profile.live_app_count, 0);
        assert_eq!(profile.captured_at_ns, 0);
        assert!(profile.is_empty());
        assert!(!profile.is_complete());
    }

    #[test]
    fn test_populated_profile_is_not_empty() {
        let profile = MachineProfile {
            live_app_count: 42,
            schema_version: 1,
            captured_at_ns: 1_000_000_000,
            sources_complete: 1,
            _pad: [0; 7],
        };
        assert!(!profile.is_empty());
        assert!(profile.is_complete());
        assert_eq!(profile.live_app_count, 42);
    }

    #[test]
    fn test_repr_c_size_is_24_bytes() {
        assert_eq!(std::mem::size_of::<MachineProfile>(), 24);
    }

    #[test]
    fn test_partial_refresh_not_complete() {
        let mut profile = MachineProfile::empty();
        profile.live_app_count = 10;
        profile.captured_at_ns = 500;
        profile.sources_complete = 0; // still partial
        assert!(!profile.is_complete());
        assert!(!profile.is_empty());
    }

    #[test]
    fn test_schema_version_default() {
        let profile = MachineProfile::empty();
        assert_eq!(profile.schema_version, 1);
    }
}
