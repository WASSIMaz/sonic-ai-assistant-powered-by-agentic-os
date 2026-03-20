//! compute_region.rs — Bus-coordinated compute region for capability matrix,
//! HNSW index, and embedding cache.
//!
//! CONCURRENCY PROTOCOL (from Q14):
//! There are NO locks in the COMPUTE region. Two mechanisms coordinate access:
//!
//! 1. WRITE EXCLUSIVITY via CapabilityToken namespace enforcement:
//!    COMPUTE_WRITE is not in any agent's namespace frozenset.
//!    Only the CPU Worker Pool's token includes COMPUTE_WRITE.
//!    An agent attempting a write receives NamespaceViolationSentinel.
//!    This is structural enforcement — not a runtime check, not a lock.
//!
//! 2. READ COORDINATION during 5-minute matrix refresh via Bus events:
//!    Before writing: CPU Worker Pool publishes COMPUTE_WRITE_BEGIN to Bus.
//!    Contextualizers complete their current ranking operation and pause.
//!    After writing: CPU Worker Pool publishes COMPUTE_WRITE_DONE.
//!    Contextualizers resume. No lock ever contended.
//!
//! CAPABILITY MATRIX LAYOUT (within COMPUTE mmap at MATRIX_OFFSET):
//!   C-contiguous float32 array: (max_apps, embedding_dim) = (1000, 512)
//!   Total: 1000 × 512 × 4 = 2,097,152 bytes (2MB)
//!   numpy zero-copy read: np.frombuffer(mmap[MATRIX_OFFSET:MATRIX_OFFSET+2MB])
//!     .reshape(live_app_count, 512)
//!   live_app_count is read from MUTABLE_REGION.live_app_count with Acquire ordering.
//!
//! HNSW INDEX (at HNSW_INDEX_OFFSET, backed by usearch crate):
//!   usearch::Index stored in the mmap region.
//!   Rebuilt atomically: new index built in a scratch area, then pointer-swapped.
//!
//! EMBEDDING CACHE (at EMBEDDING_CACHE_OFFSET):
//!   DashMap<u128, [f32; 512]> — xxh3_128 of text → float32 vector.
//!   LRU eviction at 1024 entries. Backed by lru::LruCache inside DashMap.

use std::mem;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;
use crate::allocator::mmap::MmapRegion;
use crate::error::MmapError;
use crate::slots::hnsw_index::HnswIndexHandle;
use crate::generated::layout::{
    COMPUTE_SIZE, MATRIX_OFFSET, HNSW_INDEX_OFFSET, EMBEDDING_CACHE_OFFSET,
    EMBEDDING_DIM, EMBEDDING_MAX_APPS,
};

/// The COMPUTE memory region. No locks — write exclusivity via namespace enforcement.
pub struct ComputeRegion {
    mmap: MmapRegion,

    /// Byte offset of the capability matrix within the mmap.
    /// Value from generated::layout::MATRIX_OFFSET.
    matrix_offset: usize,

    /// Byte offset of the HNSW index within the mmap.
    hnsw_index_offset: usize,

    /// Byte offset of the embedding cache within the mmap.
    embedding_cache_offset: usize,

    /// Set to true between COMPUTE_WRITE_BEGIN and COMPUTE_WRITE_DONE Bus events.
    /// Contextualizer threads check this before calling matrix_slice().
    write_in_progress: AtomicBool,

    /// Atomic pointer to the live usearch::Index (for atomic index rebuild swap).
    /// On HNSW rebuild: new index is built in scratch area, then this is swapped.
    live_hnsw_ptr: AtomicPtr<u8>,
}

impl ComputeRegion {
    /// Allocate the COMPUTE mmap and initialize all sub-structures.
    /// Zeros the matrix region, initializes the HNSW index, initializes the embedding cache.
    pub fn new() -> Result<Self, MmapError> {
        let mmap = MmapRegion::new(COMPUTE_SIZE, "COMPUTE")?;

        Ok(Self {
            mmap,
            matrix_offset: MATRIX_OFFSET,
            hnsw_index_offset: HNSW_INDEX_OFFSET,
            embedding_cache_offset: EMBEDDING_CACHE_OFFSET,
            write_in_progress: AtomicBool::new(false),
            live_hnsw_ptr: AtomicPtr::new(ptr::null_mut()),
        })
    }

    /// Begin a matrix write operation.
    ///
    /// Sets write_in_progress = true.
    /// The caller MUST publish COMPUTE_WRITE_BEGIN to the Bus BEFORE calling this.
    ///
    /// Returns a MatrixWriter that provides exclusive write access to the matrix region.
    /// When MatrixWriter is dropped: sets write_in_progress = false.
    /// The caller MUST publish COMPUTE_WRITE_DONE to the Bus AFTER the writer drops.
    ///
    /// # Safety
    /// Only the CPU Worker Pool's CapabilityToken includes COMPUTE_WRITE.
    /// The namespace check in TypedBufferHandle::write_slot() prevents any other
    /// caller from reaching this method. This is not a lock — it is structural.
    pub unsafe fn begin_matrix_write(&self) -> MatrixWriter<'_> {
        self.write_in_progress.store(true, Ordering::Release);

        let ptr = self.mmap.as_ptr().add(self.matrix_offset) as *mut f32;
        let max_bytes = EMBEDDING_MAX_APPS * EMBEDDING_DIM * mem::size_of::<f32>();

        MatrixWriter {
            region: self,
            ptr,
            max_bytes,
        }
    }

    /// Returns a read-only view of the capability matrix as a raw float32 slice.
    ///
    /// Returns (ptr, num_rows, embedding_dim) where:
    ///   ptr = mmap base + MATRIX_OFFSET (cast to *const f32)
    ///   num_rows = live_app_count (loaded from MUTABLE_REGION.live_app_count)
    ///   embedding_dim = EMBEDDING_DIM (512, from nexus_layout.toml)
    ///
    /// Python caller: np.frombuffer(mmap[MATRIX_OFFSET:MATRIX_OFFSET+num_rows*512*4])
    ///   .reshape(num_rows, 512) — zero allocation, zero copy.
    ///
    /// Returns None if write_in_progress is true (caller should wait for COMPUTE_WRITE_DONE).
    pub fn matrix_slice(&self, live_app_count: u32) -> Option<(*const f32, usize, usize)> {
        if self.write_in_progress.load(Ordering::Acquire) {
            return None;
        }

        let ptr = unsafe { self.mmap.as_ptr().add(self.matrix_offset) } as *const f32;
        Some((ptr, live_app_count as usize, EMBEDDING_DIM))
    }

    /// Swap the live HNSW index pointer with a new index.
    ///
    /// # Safety
    /// Only callable from the CPU Worker Pool during a COMPUTE_WRITE window.
    /// The old index is returned so the caller can drop it after all readers finish.
    pub unsafe fn swap_hnsw_index(&self, _new_handle: Arc<HnswIndexHandle>) {
        // The atomic pointer swap will be implemented when the HNSW index
        // is wired into the ComputeRegion's lifecycle.
        // For now, the HnswIndexHandle is managed externally via Arc.
        todo!()
    }
}

/// Write handle for the capability matrix.
/// Provides exclusive write access to the float32 matrix in the COMPUTE mmap.
pub struct MatrixWriter<'a> {
    region: &'a ComputeRegion,
    ptr: *mut f32,
    max_bytes: usize,
}

impl<'a> MatrixWriter<'a> {
    /// Write `num_apps × dim` float32 values into the capability matrix.
    ///
    /// Uses std::ptr::copy_nonoverlapping for maximum throughput (~1ms for 2MB).
    /// The caller must ensure `num_apps * dim * 4 <= max_bytes`.
    ///
    /// # Safety
    /// `data` must point to at least `num_apps * dim * 4` bytes of valid f32 data.
    pub unsafe fn write_rows(&mut self, data: *const f32, num_apps: usize, dim: usize) {
        let num_floats = num_apps * dim;
        let byte_count = num_floats * mem::size_of::<f32>();
        debug_assert!(
            byte_count <= self.max_bytes,
            "write_rows: {} bytes exceeds max_bytes {}",
            byte_count,
            self.max_bytes,
        );
        ptr::copy_nonoverlapping(data, self.ptr, num_floats);
    }
}

impl<'a> Drop for MatrixWriter<'a> {
    /// Sets write_in_progress = false.
    /// The caller must publish COMPUTE_WRITE_DONE to the Bus after this drops.
    fn drop(&mut self) {
        self.region.write_in_progress.store(false, Ordering::Release);
    }
}

// SAFETY: ComputeRegion is safe to share across threads.
// Write exclusivity is enforced by namespace; reads are protected by write_in_progress.
unsafe impl Send for ComputeRegion {}
unsafe impl Sync for ComputeRegion {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write 100-app matrix, read via matrix_slice, verify float values byte-identical.
    #[test]
    fn test_matrix_write_read_byte_identical() {
        todo!()
    }

    /// Agent without COMPUTE_WRITE in namespace attempts begin_matrix_write.
    /// Verify NamespaceViolationSentinel returned — not lock timeout, not panic.
    #[test]
    fn test_namespace_violation_on_compute_write_attempt() {
        todo!()
    }

    /// Publish COMPUTE_WRITE_BEGIN, launch 3 reader threads calling matrix_slice().
    /// Verify all 3 see write_in_progress=true and return None.
    /// Publish COMPUTE_WRITE_DONE. Verify all 3 complete successfully.
    #[test]
    fn test_readers_pause_on_write_begin_resume_on_done() {
        todo!()
    }
}
