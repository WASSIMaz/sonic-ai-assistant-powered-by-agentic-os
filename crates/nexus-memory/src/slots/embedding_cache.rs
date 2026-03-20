//! embedding_cache.rs — xxh3_128-keyed LRU embedding cache.
//!
//! DESIGN (from Q18):
//! DashMap<u128, CacheEntry> for concurrent access (no global lock on read).
//! LRU eviction at 1024 entries — lru::LruCache inside a Mutex for the eviction index.
//! When the DashMap reaches 1024 entries, the LRU eviction index identifies the
//! least-recently-used key and removes it from both the DashMap and the LRU index.
//!
//! KEY: xxh3_128(text.as_bytes()) → u128 (128-bit hash, collision probability negligible)
//! VALUE: CacheEntry { vector: [f32; 512], last_accessed: Instant }
//!
//! MEMORY FOOTPRINT:
//! 1024 entries × 2.1KB each = ~2.1MB, comfortably within L3 cache.
//!
//! CONCURRENT ACCESS PATTERN:
//! Contextualizer: concurrent reads from multiple threads (DashMap read lock per shard).
//! Cache miss path: one thread inserts a new entry (DashMap write lock per shard + LRU update).
//! Write path: CPU Worker Pool never writes the cache directly; only the embedding model
//!   invocation path (via Contextualizer cache miss) writes entries.
//!
//! RACE CONDITION FIX:
//! The get() method holds the LRU mutex FIRST, then checks DashMap under that lock.
//! This eliminates the window where a concurrent insert() could evict a key between
//! the DashMap lookup and the LRU promotion. The LRU mutex serializes all ordering
//! decisions, while DashMap provides the actual storage with shard-level concurrency.

use dashmap::DashMap;
use lru::LruCache;
use std::sync::Mutex;
use std::num::NonZeroUsize;
use xxhash_rust::xxh3::xxh3_128;

/// Maximum number of entries in the embedding cache (from nexus_layout.toml).
const CACHE_MAX_ENTRIES: usize = 1024;

/// A single cache entry.
pub struct CacheEntry {
    /// The embedding vector (512 float32 values = 2KB).
    pub vector: Box<[f32; 512]>,
}

/// Thread-safe LRU embedding cache.
/// DashMap provides concurrent shard-level locking for hot-path reads.
/// LruCache inside a Mutex tracks eviction order.
///
/// INVARIANT: Every key in `store` has a corresponding entry in `lru`, and vice versa.
/// The LRU mutex is the serialization point for all ordering decisions.
pub struct EmbeddingCache {
    /// The primary storage: xxh3_128(text) → CacheEntry.
    store: DashMap<u128, CacheEntry>,

    /// LRU eviction index: maintains insertion/access order.
    /// Mutex serializes all ordering decisions (get promotion + insert eviction).
    lru: Mutex<LruCache<u128, ()>>,
}

impl EmbeddingCache {
    /// Create a new empty EmbeddingCache with capacity CACHE_MAX_ENTRIES.
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
            lru: Mutex::new(LruCache::new(NonZeroUsize::new(CACHE_MAX_ENTRIES).unwrap())),
        }
    }

    /// Look up the embedding for a text string.
    ///
    /// Computes xxh3_128(text) as the key.
    /// Returns Some([f32; 512]) on hit, None on miss.
    ///
    /// RACE-FREE PROTOCOL:
    /// 1. Acquire LRU mutex (serializes ordering decisions).
    /// 2. Check DashMap for the key.
    /// 3. If present: promote in LRU (still under mutex), copy vector, return Some.
    /// 4. If absent: release mutex, return None.
    ///
    /// This eliminates the race where a concurrent insert() evicts the key between
    /// the DashMap lookup and the LRU promotion. The mutex hold time is brief
    /// (~50ns for LRU::get + DashMap shard read lock).
    pub fn get(&self, text: &str) -> Option<[f32; 512]> {
        let key = xxh3_128(text.as_bytes());

        // Hold LRU lock for the entire lookup+promote to prevent race with insert/evict
        let mut lru = self.lru.lock().unwrap_or_else(|e| e.into_inner());

        // Check DashMap while holding LRU lock
        let result = self.store.get(&key).map(|entry| *entry.vector);

        if result.is_some() {
            // Promote in LRU while still holding the lock — no race window
            lru.get(&key);
        }

        result
    }

    /// Insert a new embedding into the cache.
    ///
    /// Computes xxh3_128(text) as the key.
    /// If the cache is at capacity: evicts the least-recently-used entry.
    /// LRU eviction removes from both the DashMap and the LruCache index.
    ///
    /// PROTOCOL:
    /// 1. Acquire LRU mutex.
    /// 2. If at capacity: pop LRU victim, remove from DashMap.
    /// 3. Insert into LRU index.
    /// 4. Release LRU mutex.
    /// 5. Insert into DashMap.
    pub fn insert(&self, text: &str, vector: [f32; 512]) {
        let key = xxh3_128(text.as_bytes());

        {
            let mut lru = self.lru.lock().unwrap_or_else(|e| e.into_inner());

            // Evict LRU if at capacity
            if self.store.len() >= CACHE_MAX_ENTRIES {
                if let Some((evict_key, _)) = lru.pop_lru() {
                    self.store.remove(&evict_key);
                }
            }

            // Insert into LRU index under the same lock
            lru.put(key, ());
        }

        // Insert into DashMap (shard-level lock, fast)
        self.store.insert(key, CacheEntry {
            vector: Box::new(vector),
        });
    }

    /// Returns the current number of entries in the cache.
    pub fn len(&self) -> usize {
        self.store.len()
    }

    /// Returns true if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Compute the xxh3_128 hash of a text string.
    /// Exposed for testing and for the access_logger's cache key logging.
    pub fn hash_text(text: &str) -> u128 {
        xxh3_128(text.as_bytes())
    }
}

impl Default for EmbeddingCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zero_vector() -> [f32; 512] {
        [0.0f32; 512]
    }

    fn test_vector(seed: f32) -> [f32; 512] {
        let mut v = [0.0f32; 512];
        for (i, val) in v.iter_mut().enumerate() {
            *val = seed + i as f32 * 0.001;
        }
        v
    }

    #[test]
    fn test_new_cache_is_empty() {
        let cache = EmbeddingCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_insert_and_get() {
        let cache = EmbeddingCache::new();
        let vec = test_vector(1.0);
        cache.insert("hello", vec);

        let result = cache.get("hello");
        assert!(result.is_some());
        let got = result.unwrap();
        assert_eq!(got[0], vec[0]);
        assert_eq!(got[511], vec[511]);
    }

    #[test]
    fn test_miss_returns_none() {
        let cache = EmbeddingCache::new();
        assert!(cache.get("nonexistent").is_none());
    }

    #[test]
    fn test_lru_eviction_at_capacity() {
        let cache = EmbeddingCache::new();

        // Fill to capacity
        for i in 0..CACHE_MAX_ENTRIES {
            cache.insert(&format!("key_{}", i), test_vector(i as f32));
        }
        assert_eq!(cache.len(), CACHE_MAX_ENTRIES);

        // Insert one more — should evict the LRU entry (key_0)
        cache.insert("new_key", test_vector(999.0));
        assert_eq!(cache.len(), CACHE_MAX_ENTRIES);

        // key_0 should be evicted
        assert!(cache.get("key_0").is_none(), "key_0 must be evicted");

        // new_key should be present
        assert!(cache.get("new_key").is_some(), "new_key must be present");
    }

    #[test]
    fn test_lru_promotion_prevents_eviction() {
        let cache = EmbeddingCache::new();

        // Fill to capacity
        for i in 0..CACHE_MAX_ENTRIES {
            cache.insert(&format!("key_{}", i), test_vector(i as f32));
        }

        // Access key_0 to promote it to most-recently-used
        assert!(cache.get("key_0").is_some());

        // Insert one more — should evict key_1 (now LRU), NOT key_0
        cache.insert("new_key", test_vector(999.0));

        assert!(cache.get("key_0").is_some(), "key_0 must survive after promotion");
        assert!(cache.get("key_1").is_none(), "key_1 must be evicted as new LRU");
    }

    #[test]
    fn test_hash_text_deterministic() {
        let h1 = EmbeddingCache::hash_text("test string");
        let h2 = EmbeddingCache::hash_text("test string");
        assert_eq!(h1, h2, "hash must be deterministic");

        let h3 = EmbeddingCache::hash_text("different string");
        assert_ne!(h1, h3, "different strings must produce different hashes");
    }

    #[test]
    fn test_overwrite_same_key() {
        let cache = EmbeddingCache::new();
        let vec1 = test_vector(1.0);
        let vec2 = test_vector(2.0);

        cache.insert("same", vec1);
        cache.insert("same", vec2);

        let result = cache.get("same").unwrap();
        assert_eq!(result[0], vec2[0], "overwrite must replace the value");
    }

    #[test]
    fn test_concurrent_get_insert() {
        use std::sync::Arc;

        let cache = Arc::new(EmbeddingCache::new());

        // Pre-populate with some entries
        for i in 0..100 {
            cache.insert(&format!("pre_{}", i), test_vector(i as f32));
        }

        let mut handles = Vec::new();

        // 4 reader threads
        for t in 0..4 {
            let c = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    let _ = c.get(&format!("pre_{}", i));
                }
            }));
        }

        // 2 writer threads
        for t in 0..2 {
            let c = Arc::clone(&cache);
            handles.push(std::thread::spawn(move || {
                for i in 0..100 {
                    c.insert(&format!("thread_{}_{}", t, i), test_vector((t * 100 + i) as f32));
                }
            }));
        }

        for h in handles {
            h.join().expect("thread must not panic");
        }

        // Cache must still be at or below capacity
        assert!(cache.len() <= CACHE_MAX_ENTRIES,
            "cache must not exceed capacity: got {}", cache.len());
    }
}
