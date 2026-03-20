//! hnsw_index.rs — usearch::Index wrapper for in-memory ANN queries.
//!
//! PURPOSE (Layer 4, file #21 from plan):
//! usearch::Index wrapper. In-memory ANN index for capability routing.
//! Rebuilt atomically: new index built in scratch area, then pointer-swapped
//! in ComputeRegion::rebuild_hnsw(). The atomic pointer lives in ComputeRegion.
//!
//! FEATURE GATE:
//! The real usearch integration requires the "hnsw" feature flag.
//! Without it, a stub API is provided that returns errors on all operations.
//! This allows the crate to compile on systems without the C++ toolchain
//! required by usearch (e.g., Windows without full MSVC SIMD support).
//!
//! CONCURRENCY MODEL:
//! - Reads (search): concurrent, lock-free via Arc<HnswIndexHandle>.
//! - Writes (rebuild): exclusive. A new index is built in isolation, then
//!   the Arc pointer is swapped atomically. Readers on the old index complete
//!   their queries safely (Arc keeps the old index alive until dropped).

/// A single search result: key + distance from the query vector.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// The key (label) of the matched vector.
    pub key: u64,
    /// Distance from the query vector (lower = more similar for InnerProduct).
    pub distance: f32,
}

/// Errors from HNSW index operations.
#[derive(Debug)]
pub enum HnswError {
    /// Failed to initialize the usearch Index.
    InitFailed(String),
    /// Failed to reserve capacity.
    ReserveFailed(String),
    /// Vector dimension does not match index dimension.
    DimensionMismatch { expected: usize, got: usize },
    /// Index is at capacity — cannot add more vectors.
    Full { capacity: usize },
    /// Failed to add a vector to the index.
    AddFailed(String),
    /// Failed to search the index.
    SearchFailed(String),
    /// The "hnsw" feature is not enabled.
    FeatureDisabled,
}

impl std::fmt::Display for HnswError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InitFailed(e) => write!(f, "HNSW init failed: {}", e),
            Self::ReserveFailed(e) => write!(f, "HNSW reserve failed: {}", e),
            Self::DimensionMismatch { expected, got } => {
                write!(f, "HNSW dimension mismatch: expected {}, got {}", expected, got)
            }
            Self::Full { capacity } => write!(f, "HNSW index full: capacity {}", capacity),
            Self::AddFailed(e) => write!(f, "HNSW add failed: {}", e),
            Self::SearchFailed(e) => write!(f, "HNSW search failed: {}", e),
            Self::FeatureDisabled => write!(f, "HNSW feature not enabled: compile with --features hnsw"),
        }
    }
}

impl std::error::Error for HnswError {}

// ============================================================================
// Real implementation (with usearch)
// ============================================================================

#[cfg(feature = "hnsw")]
mod inner {
    use super::*;
    use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

    /// Default HNSW connectivity parameter (M).
    const HNSW_CONNECTIVITY: usize = 16;

    /// Construction-time expansion factor (ef_construction).
    const HNSW_EXPANSION_ADD: usize = 128;

    /// Query-time expansion factor (ef).
    const HNSW_EXPANSION_SEARCH: usize = 64;

    /// Wrapper around a usearch::Index for ANN queries over capability embeddings.
    pub struct HnswIndexHandle {
        index: Index,
        dim: usize,
        capacity: usize,
    }

    impl HnswIndexHandle {
        /// Create a new empty HNSW index with the given dimension and capacity.
        pub fn new(dim: usize, capacity: usize) -> Result<Self, HnswError> {
            let opts = IndexOptions {
                dimensions: dim,
                metric: MetricKind::IP,
                quantization: ScalarKind::F32,
                connectivity: HNSW_CONNECTIVITY,
                expansion_add: HNSW_EXPANSION_ADD,
                expansion_search: HNSW_EXPANSION_SEARCH,
                multi: false,
            };

            let index = Index::new(&opts).map_err(|e| HnswError::InitFailed(e.to_string()))?;
            index
                .reserve(capacity)
                .map_err(|e| HnswError::ReserveFailed(e.to_string()))?;

            Ok(Self { index, dim, capacity })
        }

        /// Add a labeled vector to the index.
        pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), HnswError> {
            if vector.len() != self.dim {
                return Err(HnswError::DimensionMismatch {
                    expected: self.dim,
                    got: vector.len(),
                });
            }
            if self.index.size() >= self.capacity {
                return Err(HnswError::Full { capacity: self.capacity });
            }
            self.index.add(key, vector).map_err(|e| HnswError::AddFailed(e.to_string()))?;
            Ok(())
        }

        /// Search for the top-k nearest neighbors.
        pub fn search(&self, query: &[f32], count: usize) -> Result<Vec<SearchResult>, HnswError> {
            if query.len() != self.dim {
                return Err(HnswError::DimensionMismatch {
                    expected: self.dim,
                    got: query.len(),
                });
            }
            if self.index.size() == 0 {
                return Ok(Vec::new());
            }
            let results = self.index.search(query, count)
                .map_err(|e| HnswError::SearchFailed(e.to_string()))?;
            Ok(results.keys.into_iter().zip(results.distances.into_iter())
                .map(|(key, distance)| SearchResult { key, distance })
                .collect())
        }

        /// Build a fresh index from a dense matrix of vectors.
        pub fn rebuild(dim: usize, capacity: usize, vectors: &[f32]) -> Result<Self, HnswError> {
            let num_vectors = vectors.len() / dim;
            if vectors.len() % dim != 0 {
                return Err(HnswError::DimensionMismatch {
                    expected: dim,
                    got: vectors.len() % dim,
                });
            }
            let handle = Self::new(dim, capacity)?;
            for i in 0..num_vectors {
                let start = i * dim;
                let end = start + dim;
                handle.add(i as u64, &vectors[start..end])?;
            }
            Ok(handle)
        }

        pub fn len(&self) -> usize { self.index.size() }
        pub fn is_empty(&self) -> bool { self.index.size() == 0 }
        pub fn capacity(&self) -> usize { self.capacity }
        pub fn dim(&self) -> usize { self.dim }
    }

    unsafe impl Send for HnswIndexHandle {}
    unsafe impl Sync for HnswIndexHandle {}
}

// ============================================================================
// Stub implementation (without usearch)
// ============================================================================

#[cfg(not(feature = "hnsw"))]
mod inner {
    use super::*;

    /// Stub HNSW index handle when the "hnsw" feature is not enabled.
    ///
    /// All mutation/query operations return `HnswError::FeatureDisabled`.
    /// This allows the rest of the crate to compile and test without usearch.
    pub struct HnswIndexHandle {
        dim: usize,
        capacity: usize,
        count: std::sync::atomic::AtomicUsize,
        /// In-memory storage for vectors (used for basic functionality without usearch).
        /// Each entry is (key, vector). Linear scan for search.
        entries: std::sync::Mutex<Vec<(u64, Vec<f32>)>>,
    }

    impl HnswIndexHandle {
        /// Create a new stub HNSW index.
        /// Works without usearch by using a simple linear-scan backing store.
        pub fn new(dim: usize, capacity: usize) -> Result<Self, HnswError> {
            Ok(Self {
                dim,
                capacity,
                count: std::sync::atomic::AtomicUsize::new(0),
                entries: std::sync::Mutex::new(Vec::with_capacity(capacity.min(1024))),
            })
        }

        /// Add a labeled vector (linear scan fallback).
        pub fn add(&self, key: u64, vector: &[f32]) -> Result<(), HnswError> {
            if vector.len() != self.dim {
                return Err(HnswError::DimensionMismatch {
                    expected: self.dim,
                    got: vector.len(),
                });
            }
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries.len() >= self.capacity {
                return Err(HnswError::Full { capacity: self.capacity });
            }
            entries.push((key, vector.to_vec()));
            self.count.store(entries.len(), std::sync::atomic::Ordering::Release);
            Ok(())
        }

        /// Search by brute-force inner product (linear scan fallback).
        pub fn search(&self, query: &[f32], count: usize) -> Result<Vec<SearchResult>, HnswError> {
            if query.len() != self.dim {
                return Err(HnswError::DimensionMismatch {
                    expected: self.dim,
                    got: query.len(),
                });
            }
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if entries.is_empty() {
                return Ok(Vec::new());
            }

            // Compute inner product distances
            let mut scored: Vec<(u64, f32)> = entries.iter().map(|(key, vec)| {
                let ip: f32 = query.iter().zip(vec.iter()).map(|(a, b)| a * b).sum();
                // usearch InnerProduct: lower distance = higher similarity
                // Convert to distance: 1.0 - ip (for normalized vectors, ip in [0, 1])
                (*key, 1.0 - ip)
            }).collect();

            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(count);

            Ok(scored.into_iter()
                .map(|(key, distance)| SearchResult { key, distance })
                .collect())
        }

        /// Build a fresh index from a dense matrix of vectors.
        pub fn rebuild(dim: usize, capacity: usize, vectors: &[f32]) -> Result<Self, HnswError> {
            let num_vectors = vectors.len() / dim;
            if vectors.len() % dim != 0 {
                return Err(HnswError::DimensionMismatch {
                    expected: dim,
                    got: vectors.len() % dim,
                });
            }
            let handle = Self::new(dim, capacity)?;
            for i in 0..num_vectors {
                let start = i * dim;
                let end = start + dim;
                handle.add(i as u64, &vectors[start..end])?;
            }
            Ok(handle)
        }

        pub fn len(&self) -> usize {
            self.count.load(std::sync::atomic::Ordering::Acquire)
        }
        pub fn is_empty(&self) -> bool { self.len() == 0 }
        pub fn capacity(&self) -> usize { self.capacity }
        pub fn dim(&self) -> usize { self.dim }
    }

    unsafe impl Send for HnswIndexHandle {}
    unsafe impl Sync for HnswIndexHandle {}
}

// Re-export the active implementation
pub use inner::HnswIndexHandle;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const TEST_DIM: usize = 8;
    const TEST_CAPACITY: usize = 100;

    fn normalized_vector(seed: f32, dim: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dim).map(|i| seed + i as f32 * 0.1).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
        v
    }

    #[test]
    fn test_new_index_is_empty() {
        let handle = HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
            .expect("index creation must succeed");
        assert!(handle.is_empty());
        assert_eq!(handle.len(), 0);
        assert_eq!(handle.dim(), TEST_DIM);
        assert_eq!(handle.capacity(), TEST_CAPACITY);
    }

    #[test]
    fn test_add_and_search() {
        let handle = HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
            .expect("index creation must succeed");

        let v0 = normalized_vector(1.0, TEST_DIM);
        let v1 = normalized_vector(2.0, TEST_DIM);
        let v2 = normalized_vector(100.0, TEST_DIM);

        handle.add(0, &v0).unwrap();
        handle.add(1, &v1).unwrap();
        handle.add(2, &v2).unwrap();

        assert_eq!(handle.len(), 3);

        // Search for v0 — should find v0 first (exact match)
        let results = handle.search(&v0, 3).unwrap();
        assert!(!results.is_empty(), "search must return results");
        assert_eq!(results[0].key, 0, "exact match must be first result");
    }

    #[test]
    fn test_dimension_mismatch_on_add() {
        let handle = HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
            .expect("index creation must succeed");

        let wrong_dim = vec![1.0f32; TEST_DIM + 1];
        let result = handle.add(0, &wrong_dim);
        assert!(matches!(result, Err(HnswError::DimensionMismatch { .. })));
    }

    #[test]
    fn test_dimension_mismatch_on_search() {
        let handle = HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
            .expect("index creation must succeed");

        handle.add(0, &normalized_vector(1.0, TEST_DIM)).unwrap();

        let wrong_dim = vec![1.0f32; TEST_DIM + 1];
        let result = handle.search(&wrong_dim, 1);
        assert!(matches!(result, Err(HnswError::DimensionMismatch { .. })));
    }

    #[test]
    fn test_search_empty_index_returns_empty() {
        let handle = HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
            .expect("index creation must succeed");

        let query = normalized_vector(1.0, TEST_DIM);
        let results = handle.search(&query, 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_rebuild_from_matrix() {
        let num_vectors = 10;
        let dim = TEST_DIM;

        let mut matrix = Vec::with_capacity(num_vectors * dim);
        for i in 0..num_vectors {
            let v = normalized_vector(i as f32 * 10.0, dim);
            matrix.extend_from_slice(&v);
        }

        let handle = HnswIndexHandle::rebuild(dim, TEST_CAPACITY, &matrix)
            .expect("rebuild must succeed");

        assert_eq!(handle.len(), num_vectors);

        let query = normalized_vector(0.0, dim);
        let results = handle.search(&query, 1).unwrap();
        assert_eq!(results[0].key, 0);
    }

    #[test]
    fn test_rebuild_dimension_mismatch() {
        let matrix = vec![1.0f32; 17]; // not divisible by 8
        let result = HnswIndexHandle::rebuild(TEST_DIM, TEST_CAPACITY, &matrix);
        assert!(matches!(result, Err(HnswError::DimensionMismatch { .. })));
    }

    #[test]
    fn test_full_index_returns_error() {
        let small_capacity = 3;
        let handle = HnswIndexHandle::new(TEST_DIM, small_capacity)
            .expect("index creation must succeed");

        for i in 0..small_capacity {
            handle.add(i as u64, &normalized_vector(i as f32, TEST_DIM)).unwrap();
        }

        let result = handle.add(99, &normalized_vector(99.0, TEST_DIM));
        assert!(matches!(result, Err(HnswError::Full { .. })));
    }

    #[test]
    fn test_concurrent_search() {
        let handle = Arc::new(
            HnswIndexHandle::new(TEST_DIM, TEST_CAPACITY)
                .expect("index creation must succeed")
        );

        for i in 0..50 {
            handle.add(i, &normalized_vector(i as f32, TEST_DIM)).unwrap();
        }

        let mut handles = Vec::new();
        for _ in 0..4 {
            let idx = Arc::clone(&handle);
            handles.push(std::thread::spawn(move || {
                for i in 0..50 {
                    let query = normalized_vector(i as f32, TEST_DIM);
                    let results = idx.search(&query, 5).unwrap();
                    assert!(!results.is_empty(), "concurrent search must return results");
                }
            }));
        }

        for h in handles {
            h.join().expect("search thread must not panic");
        }
    }
}
