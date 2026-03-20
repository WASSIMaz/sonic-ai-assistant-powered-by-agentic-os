//! machine_profile_store.rs — Frozen MachineProfile with 5-minute refresh.
//!
//! PURPOSE (Layer 7, file #33 from plan):
//! Frozen MachineProfile. 5-minute background refresh cycle.
//! Triggers capability matrix write in ComputeRegion on refresh.
//! ArcSwap<MachineProfile> for lock-free atomic replacement.

use arc_swap::ArcSwap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::slots::machine_profile::MachineProfile;

/// Hot store for the frozen MachineProfile.
/// Refreshed every COMPUTE_REFRESH_MIN (5) minutes by a background thread.
pub struct MachineProfileStore {
    /// The current live MachineProfile snapshot.
    /// ArcSwap provides atomic load/store with no lock on the read path.
    current: ArcSwap<MachineProfile>,
}

impl MachineProfileStore {
    /// Create a new store with an empty initial profile.
    /// The first 5-minute refresh cycle will replace this.
    pub fn new() -> Self {
        Self {
            current: ArcSwap::from(Arc::new(MachineProfile::empty())),
        }
    }

    /// Store a new MachineProfile snapshot atomically.
    /// Called by the background refresh thread after each 5-minute cycle.
    pub fn store(&self, profile: Arc<MachineProfile>) {
        self.current.store(profile);
    }

    /// Load the current MachineProfile.
    /// Returns a Guard<Arc<MachineProfile>> — snapshot is protected from
    /// reclamation for the duration of the Guard's lifetime.
    pub fn load(&self) -> arc_swap::Guard<Arc<MachineProfile>> {
        self.current.load()
    }

    /// Spawn the 5-minute background refresh thread.
    ///
    /// On each cycle:
    /// 1. Detect installed applications via OS APIs.
    /// 2. Build new MachineProfile with app_names and descriptions.
    /// 3. Update live_app_count in MUTABLE_REGION.
    /// 4. Trigger capability matrix embed and write in ComputeRegion.
    /// 5. store(Arc::new(new_profile)).
    ///
    /// The refresh_fn closure provides the platform-specific app detection.
    /// Called every COMPUTE_REFRESH_MIN (5) minutes.
    pub fn spawn_refresh_thread<F>(store: Arc<Self>, refresh_fn: F)
    where
        F: Fn() -> MachineProfile + Send + 'static,
    {
        use crate::generated::layout::COMPUTE_REFRESH_MIN;
        let refresh_interval = Duration::from_secs(COMPUTE_REFRESH_MIN * 60);

        std::thread::Builder::new()
            .name("nexus-machine-profile-refresh".to_string())
            .spawn(move || {
                let mut last_refresh = Instant::now();
                loop {
                    // Wait until the next 5-minute mark
                    let elapsed = last_refresh.elapsed();
                    if elapsed < refresh_interval {
                        std::thread::sleep(refresh_interval - elapsed);
                    }
                    last_refresh = Instant::now();

                    // Perform the refresh
                    let new_profile = refresh_fn();
                    store.store(Arc::new(new_profile));
                }
            })
            .expect("machine profile refresh thread spawn failed");
    }
}

impl Default for MachineProfileStore {
    fn default() -> Self {
        Self::new()
    }
}
