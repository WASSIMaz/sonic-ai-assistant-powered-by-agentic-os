//! resident_embedding_model.rs — Single resident embedding model instance.
//!
//! BACKEND SELECTION (from Q3):
//! At startup, the system detects available runtimes in priority order:
//!   CUDA → CoreML → ONNX(gpu) → ONNX(cpu) → CPU
//! The first available backend is selected and loaded. The selected backend
//! is logged at startup for diagnostics. If no GPU accelerator is available,
//! the CPU backend is used as fallback — model loading is always testable.
//!
//! SINGLETON:
//! Exactly ONE model instance is loaded at startup. All embedding requests
//! go through this single instance. Thread-safe via Arc<Mutex<Backend>>.
//!
//! OUTPUT:
//! Returns float32[512] vectors (dimension fixed in nexus_layout.toml).
//! All backends must produce identical output for the same input text
//! (deterministic for cache correctness).
//!
//! EMBEDDING CACHE INTEGRATION:
//! The embedding cache in ComputeRegion holds the 1024 most recently used
//! embeddings keyed by xxh3_128(text). The model is only invoked on a cache miss.

use std::sync::{Arc, Mutex};
use crate::generated::layout::MATRIX_EMBEDDING_DIM;

/// The selected embedding backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingBackend {
    Cuda,
    CoreMl,
    OnnxGpu,
    OnnxCpu,
    Cpu,
}

/// The resident embedding model. Loaded once at startup.
pub struct ResidentEmbeddingModel {
    /// Which backend was selected at startup.
    pub backend: EmbeddingBackend,

    /// The inner model — backend-specific implementation behind a trait object.
    inner: Arc<Mutex<dyn EmbeddingModelBackend + Send>>,

    /// Embedding output dimension (512 from nexus_layout.toml).
    pub dim: usize,
}

impl ResidentEmbeddingModel {
    /// Detect available backends and load the best one.
    ///
    /// Priority: CUDA → CoreML → ONNX(gpu) → ONNX(cpu) → CPU.
    /// Logs the selected backend at INFO level.
    /// Returns an error only if ALL backends fail to load — CPU never fails.
    pub fn load() -> Result<Self, EmbeddingModelError> {
        todo!()
    }

    /// Embed a single text string into a float32[512] vector.
    ///
    /// Thread-safe: acquires the inner Mutex for the duration of inference.
    /// Callers should check the EmbeddingCache before calling this.
    pub fn embed(&self, text: &str) -> Result<[f32; MATRIX_EMBEDDING_DIM], EmbeddingModelError> {
        todo!()
    }

    /// Embed a batch of texts, returning one float32[512] per input.
    ///
    /// Batching improves GPU utilization. The CPU Worker Pool uses this
    /// during MachineProfile refresh to embed all app capability descriptions.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<[f32; MATRIX_EMBEDDING_DIM]>, EmbeddingModelError> {
        todo!()
    }
}

/// Trait implemented by each backend (CUDA, CoreML, ONNX, CPU).
trait EmbeddingModelBackend {
    fn embed(&mut self, text: &str) -> Result<Vec<f32>, EmbeddingModelError>;
    fn embed_batch(&mut self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingModelError>;
    fn backend_name(&self) -> &'static str;
}

/// Errors from embedding model operations.
#[derive(Debug, thiserror::Error)]
pub enum EmbeddingModelError {
    #[error("backend load failed: {backend}, reason: {reason}")]
    LoadFailed { backend: &'static str, reason: String },

    #[error("inference failed: {reason}")]
    InferenceFailed { reason: String },

    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
}