//! curated_facts_store.rs — MEMORY.md loaded into frozen no-decay struct.
//!
//! PURPOSE (Layer 7, file #35 from plan):
//! MEMORY.md content loaded into a frozen struct at startup.
//! Never evicted. Never mutated after initial load.
//! Provides the agent's long-term memory reference without going to disk.

use std::sync::Arc;

/// The curated facts store — holds MEMORY.md content in memory permanently.
/// Loaded once at startup. Immutable after construction.
/// All readers share the same Arc — no copy on access.
pub struct CuratedFactsStore {
    /// The full content of MEMORY.md, loaded at startup.
    /// None if the file does not exist (acceptable — treat as empty memory).
    content: Option<Arc<String>>,

    /// The path from which the content was loaded, for logging.
    source_path: String,
}

impl CuratedFactsStore {
    /// Load MEMORY.md from the given path.
    ///
    /// If the file does not exist: returns a store with content = None.
    /// This is NOT an error — fresh deployments may not have a MEMORY.md.
    ///
    /// If the file exists but is not valid UTF-8: returns a store with
    /// content = None and logs a warning. MEMORY.md must be UTF-8.
    pub fn load(path: &str) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(text) => {
                Some(Arc::new(text))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Not an error — fresh deployment
                None
            }
            Err(_) => {
                // Read error or encoding error — treat as empty
                None
            }
        };

        Self {
            content,
            source_path: path.to_string(),
        }
    }

    /// Returns the full MEMORY.md content, or None if not loaded.
    /// Arc clone — O(1), no copy of the string data.
    pub fn get(&self) -> Option<Arc<String>> {
        self.content.clone()
    }

    /// Returns true if MEMORY.md was successfully loaded.
    pub fn is_loaded(&self) -> bool {
        self.content.is_some()
    }

    /// Returns the source path this store was loaded from.
    pub fn source_path(&self) -> &str {
        &self.source_path
    }
}

impl Default for CuratedFactsStore {
    fn default() -> Self {
        Self::load("MEMORY.md")
    }
}
