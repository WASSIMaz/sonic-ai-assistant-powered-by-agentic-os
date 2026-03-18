//! # Error Types for NEXUS Utilities
//!
//! This module provides comprehensive error handling primitives used throughout
//! the NEXUS Agentic Operating System. The error types are designed for:
//!
//! - Zero-allocation error propagation where possible
//! - Rich context for debugging
//! - Integration with `std::error::Error` trait hierarchy
//! - Thread-safe error handling with `Send + Sync` bounds
//!
//! ## Usage
//!
//! ```rust
//! use nexus_utils::error::{NexusError, NexusResult, ErrorKind};
//!
//! fn example_function() -> NexusResult<()> {
//!     // Create an error with context
//!     let error = NexusError::new(ErrorKind::InvalidInput, "invalid value");
//!     Err(error)
//! }
//!
//! let result = example_function();
//! assert!(result.is_err());
//! ```

use std::any::Any;
use std::backtrace::Backtrace;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// The canonical result type for NEXUS utilities.
///
/// Uses `Arc<NexusError>` to enable efficient cloning and sharing across threads
/// without requiring `Clone` bounds on potentially large error payloads.
pub type NexusResult<T> = std::result::Result<T, NexusError>;

/// A thread-safe, cloneable error type with rich context and backtrace support.
///
/// ## Design Philosophy
///
/// - **Cheap to Clone**: Uses `Arc` internally, making clones O(1)
/// - **Thread-Safe**: Implements `Send + Sync` for cross-thread error propagation
/// - **Rich Context**: Stores error chain, custom context, and optional backtraces
/// - **Type-Erased**: Can hold any underlying error type while maintaining type info
#[derive(Debug, Clone)]
pub struct NexusError {
    /// The underlying error category
    kind: ErrorKind,
    /// Human-readable error message
    message: Cow<'static, str>,
    /// Optional source error for error chains
    source: Option<Arc<NexusError>>,
    /// Optional backtrace captured at creation time
    backtrace: Option<Arc<Backtrace>>,
    /// Additional context as type-erased data
    context: Option<Arc<dyn Any + Send + Sync>>,
}

impl NexusError {
    /// Creates a new `NexusError` with the given kind and message.
    ///
    /// # Example
    ///
    /// ```rust
    /// use nexus_utils::error::{NexusError, ErrorKind};
    ///
    /// let error = NexusError::new(ErrorKind::InvalidInput, "invalid parameter value");
    /// ```
    #[inline]
    pub fn new<M: Into<Cow<'static, str>>>(kind: ErrorKind, message: M) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            backtrace: Some(Arc::new(Backtrace::capture())),
            context: None,
        }
    }

    /// Creates a new error with backtrace capture disabled.
    ///
    /// Use this for hot paths where backtrace capture overhead is unacceptable.
    #[inline]
    pub fn new_no_backtrace<M: Into<Cow<'static, str>>>(kind: ErrorKind, message: M) -> Self {
        Self {
            kind,
            message: message.into(),
            source: None,
            backtrace: None,
            context: None,
        }
    }

    /// Adds a source error to create an error chain.
    ///
    /// # Example
    ///
    /// ```rust
    /// use nexus_utils::error::{NexusError, ErrorKind};
    ///
    /// let inner = NexusError::new(ErrorKind::Io, "file not found");
    /// let outer = inner.with_source(NexusError::new(ErrorKind::Config, "failed to load config"));
    /// ```
    #[inline]
    pub fn with_source(mut self, source: NexusError) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    /// Adds context data to this error.
    ///
    /// The context can be retrieved using `get_context::<T>()`.
    #[inline]
    pub fn with_context<T: Any + Send + Sync>(mut self, context: T) -> Self {
        self.context = Some(Arc::new(context));
        self
    }

    /// Returns the error kind.
    #[inline]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Returns the error message.
    #[inline]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the source error if present.
    #[inline]
    pub fn source_error(&self) -> Option<&NexusError> {
        self.source.as_ref().map(|arc| arc.as_ref())
    }

    /// Returns the backtrace if captured.
    #[inline]
    pub fn backtrace(&self) -> Option<&Backtrace> {
        self.backtrace.as_ref().map(|arc| arc.as_ref())
    }

    /// Retrieves context data of the specified type.
    ///
    /// Returns `None` if the context is not of type `T` or if no context exists.
    #[inline]
    pub fn get_context<T: Any>(&self) -> Option<&T> {
        self.context.as_ref().and_then(|c| c.downcast_ref::<T>())
    }

    /// Returns the full error chain as a vector.
    ///
    /// The first element is this error, followed by its source, then the source's source, etc.
    pub fn chain(&self) -> Vec<&NexusError> {
        let mut chain = vec![self];
        let mut current = self.source_error();
        while let Some(err) = current {
            chain.push(err);
            current = err.source_error();
        }
        chain
    }

    /// Returns `true` if this error or any source error matches the given kind.
    pub fn has_kind(&self, kind: ErrorKind) -> bool {
        if self.kind == kind {
            return true;
        }
        self.source_error().map_or(false, |s| s.has_kind(kind))
    }
}

impl fmt::Display for NexusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(source) = &self.source {
            write!(f, "\n  caused by: {}", source)?;
        }
        if let Some(bt) = &self.backtrace {
            write!(f, "\n\nBacktrace:\n{}", bt)?;
        }
        Ok(())
    }
}

impl std::error::Error for NexusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|arc| arc.as_ref() as &(dyn std::error::Error + 'static))
    }
}

/// Categories of errors in NEXUS.
///
/// Error kinds are organized by subsystem and severity, enabling
/// programmatic error handling based on error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum ErrorKind {
    // ============================================================================
    // General Errors
    // ============================================================================

    /// The input provided was invalid or malformed.
    #[error("Invalid input")]
    InvalidInput,

    /// A required parameter was missing.
    #[error("Missing parameter")]
    MissingParameter,

    /// An operation was attempted on an invalid state.
    #[error("Invalid state")]
    InvalidState,

    /// An operation exceeded its allowed bounds.
    #[error("Out of bounds")]
    OutOfBounds,

    /// The requested resource was not found.
    #[error("Not found")]
    NotFound,

    /// The requested entity already exists.
    #[error("Already exists")]
    AlreadyExists,

    // ============================================================================
    // Concurrency Errors
    // ============================================================================

    /// A lock acquisition failed due to contention.
    #[error("Lock contention")]
    LockContention,

    /// A timeout occurred while waiting for a resource.
    #[error("Timeout")]
    Timeout,

    /// A deadlock was detected.
    #[error("Deadlock detected")]
    Deadlock,

    /// Channel send operation failed (receiver dropped).
    #[error("Channel send error")]
    ChannelSend,

    /// Channel receive operation failed (sender dropped).
    #[error("Channel receive error")]
    ChannelReceive,

    /// Capacity limit exceeded.
    #[error("Capacity exceeded")]
    CapacityExceeded,

    // ============================================================================
    // Timing Errors
    // ============================================================================

    /// Timer drift exceeded acceptable threshold.
    #[error("Timer drift")]
    TimerDrift,

    /// Heartbeat missed.
    #[error("Heartbeat missed")]
    HeartbeatMissed,

    /// Clock synchronization failure.
    #[error("Clock sync error")]
    ClockSync,

    /// Invalid timestamp.
    #[error("Invalid timestamp")]
    InvalidTimestamp,

    // ============================================================================
    // Data Structure Errors
    // ============================================================================

    /// Ring buffer is empty.
    #[error("Buffer empty")]
    BufferEmpty,

    /// Ring buffer is full.
    #[error("Buffer full")]
    BufferFull,

    /// Invalid buffer position.
    #[error("Invalid position")]
    InvalidPosition,

    /// Hash map operation failed.
    #[error("Hash map error")]
    HashMapError,

    // ============================================================================
    // Memory Errors
    // ============================================================================

    /// Memory allocation failed.
    #[error("Allocation failed")]
    AllocationFailed,

    /// Memory layout is invalid.
    #[error("Invalid layout")]
    InvalidLayout,

    /// Cache miss.
    #[error("Cache miss")]
    CacheMiss,

    // ============================================================================
    // I/O Errors
    // ============================================================================

    /// I/O operation failed.
    #[error("I/O error")]
    Io,

    /// Configuration error.
    #[error("Configuration error")]
    Config,

    /// Serialization/deserialization error.
    #[error("Serialization error")]
    Serialization,

    // ============================================================================
    // System Errors
    // ============================================================================

    /// System call failed.
    #[error("System error")]
    SystemError,

    /// Permission denied.
    #[error("Permission denied")]
    PermissionDenied,

    /// Resource exhausted.
    #[error("Resource exhausted")]
    ResourceExhausted,

    /// Interrupted operation.
    #[error("Interrupted")]
    Interrupted,

    // ============================================================================
    // Internal Errors
    // ============================================================================

    /// An invariant was violated.
    #[error("Invariant violation")]
    InvariantViolation,

    /// An internal assertion failed.
    #[error("Internal error")]
    InternalError,

    /// Feature not implemented.
    #[error("Not implemented")]
    NotImplemented,
}

impl ErrorKind {
    /// Returns `true` if this error kind represents a recoverable error.
    ///
    /// Recoverable errors can potentially be handled and the operation retried.
    #[inline]
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            ErrorKind::Timeout
                | ErrorKind::LockContention
                | ErrorKind::CapacityExceeded
                | ErrorKind::ResourceExhausted
                | ErrorKind::Interrupted
                | ErrorKind::CacheMiss
        )
    }

    /// Returns `true` if this error kind indicates a transient condition.
    ///
    /// Transient errors may resolve themselves if the operation is retried after a delay.
    #[inline]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ErrorKind::Timeout | ErrorKind::LockContention | ErrorKind::Interrupted
        )
    }

    /// Returns `true` if this error kind indicates a fatal condition.
    ///
    /// Fatal errors cannot be recovered and require system-level intervention.
    #[inline]
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            ErrorKind::Deadlock
                | ErrorKind::InvariantViolation
                | ErrorKind::AllocationFailed
                | ErrorKind::SystemError
        )
    }

    /// Returns the severity level for this error kind.
    #[inline]
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            ErrorKind::InvalidInput
            | ErrorKind::MissingParameter
            | ErrorKind::NotFound
            | ErrorKind::AlreadyExists => ErrorSeverity::Warning,

            ErrorKind::LockContention
            | ErrorKind::Timeout
            | ErrorKind::CapacityExceeded
            | ErrorKind::BufferEmpty
            | ErrorKind::CacheMiss => ErrorSeverity::Info,

            ErrorKind::InvalidState
            | ErrorKind::OutOfBounds
            | ErrorKind::Deadlock
            | ErrorKind::ChannelSend
            | ErrorKind::ChannelReceive
            | ErrorKind::TimerDrift
            | ErrorKind::HeartbeatMissed
            | ErrorKind::ClockSync
            | ErrorKind::InvalidTimestamp
            | ErrorKind::BufferFull
            | ErrorKind::InvalidPosition
            | ErrorKind::HashMapError
            | ErrorKind::AllocationFailed
            | ErrorKind::InvalidLayout
            | ErrorKind::Io
            | ErrorKind::Config
            | ErrorKind::Serialization
            | ErrorKind::SystemError
            | ErrorKind::PermissionDenied
            | ErrorKind::ResourceExhausted
            | ErrorKind::Interrupted => ErrorSeverity::Error,

            ErrorKind::InvariantViolation
            | ErrorKind::InternalError
            | ErrorKind::NotImplemented => ErrorSeverity::Critical,
        }
    }
}

/// Severity levels for errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorSeverity {
    /// Informational - no action required.
    Info,
    /// Warning - may require attention.
    Warning,
    /// Error - operation failed, recovery possible.
    Error,
    /// Critical - system integrity at risk.
    Critical,
}

impl fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorSeverity::Info => write!(f, "INFO"),
            ErrorSeverity::Warning => write!(f, "WARN"),
            ErrorSeverity::Error => write!(f, "ERROR"),
            ErrorSeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Extension trait for adding context to results.
///
/// This trait is implemented for `Result<T, E>` where `E` can be converted
/// to `NexusError`. It provides ergonomic methods for adding context.
pub trait ResultExt<T> {
    /// Adds context to the error if the result is `Err`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use nexus_utils::error::{NexusError, ResultExt, ErrorKind};
    ///
    /// fn operation() -> Result<(), NexusError> {
    ///     Err(NexusError::new(ErrorKind::Io, "read failed"))
    /// }
    ///
    /// let result = operation().context("while loading configuration");
    /// assert!(result.is_err());
    /// // The context is added as part of the error chain
    /// ```
    fn context<M: Into<Cow<'static, str>> + Send + Sync + 'static>(self, message: M) -> NexusResult<T>;

    /// Adds context with lazy evaluation.
    ///
    /// The closure is only called if the result is `Err`.
    fn with_context<M, F>(self, f: F) -> NexusResult<T>
    where
        M: Into<Cow<'static, str>> + Send + Sync + 'static,
        F: FnOnce() -> M;
}

impl<T, E: Into<NexusError>> ResultExt<T> for std::result::Result<T, E> {
    #[inline]
    fn context<M: Into<Cow<'static, str>> + Send + Sync + 'static>(self, message: M) -> NexusResult<T> {

        self.map_err(|e| e.into().with_context(message))
    }

    #[inline]
    fn with_context<M, F>(self, f: F) -> NexusResult<T>
    where
        M: Into<Cow<'static, str>> + Send + Sync + 'static,
        F: FnOnce() -> M,
    {
        self.map_err(|e| e.into().with_context(f()))
    }
}

impl From<std::io::Error> for NexusError {
    #[inline]
    fn from(err: std::io::Error) -> Self {
        NexusError::new(ErrorKind::Io, err.to_string()).with_source(NexusError::new_no_backtrace(ErrorKind::Io, err.to_string()))
    }
}

impl From<std::fmt::Error> for NexusError {
    #[inline]
    fn from(err: std::fmt::Error) -> Self {
        NexusError::new(ErrorKind::InternalError, err.to_string())
    }
}

impl From<String> for NexusError {
    #[inline]
    fn from(message: String) -> Self {
        NexusError::new(ErrorKind::InternalError, message)
    }
}

impl From<&'static str> for NexusError {
    #[inline]
    fn from(message: &'static str) -> Self {
        NexusError::new(ErrorKind::InternalError, message)
    }
}

// ============================================================================
// Architecture-Specific Error Types
// ============================================================================

/// Type alias for slot identifiers in typed buffers.
pub type SlotId = u64;

/// Type alias for access operations on typed buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessOp {
    /// Read operation on a buffer slot.
    Read,
    /// Write operation on a buffer slot.
    Write,
    /// Append operation on a buffer slot.
    Append,
    /// Compute operation on a buffer slot.
    Compute,
}

impl fmt::Display for AccessOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AccessOp::Read => write!(f, "read"),
            AccessOp::Write => write!(f, "write"),
            AccessOp::Append => write!(f, "append"),
            AccessOp::Compute => write!(f, "compute"),
        }
    }
}

/// Error indicating a lock acquisition timed out with holder information.
///
/// This error is raised when a timed lock acquisition fails and includes
/// detailed information about which agent holds the lock, enabling better
/// debugging and deadlock detection in the cognitive kernel.
#[derive(Debug, Clone)]
pub struct LockTimeoutError {
    /// The slot identifier (for TypedBuffer slots) or name (for named locks).
    pub slot_id: SlotId,
    /// Human-readable slot name for debugging and logging.
    pub slot_name: Cow<'static, str>,
    /// The agent ID of the current lock holder, if known.
    pub holder_agent_id: Option<u64>,
    /// The thread ID of the current lock holder, if known.
    /// Used by L1 watchdog to identify stalled threads for soft recovery.
    pub holder_thread_id: Option<u64>,
    /// How long we waited before timing out.
    pub timeout_duration: std::time::Duration,
}

impl LockTimeoutError {
    /// Creates a new lock timeout error.
    #[inline]
    pub fn new(slot_name: impl Into<Cow<'static, str>>, timeout_duration: std::time::Duration) -> Self {
        Self {
            slot_id: 0,
            slot_name: slot_name.into(),
            holder_agent_id: None,
            holder_thread_id: None,
            timeout_duration,
        }
    }

    /// Sets the slot ID.
    #[inline]
    pub fn with_slot_id(mut self, slot_id: SlotId) -> Self {
        self.slot_id = slot_id;
        self
    }

    /// Sets the holder agent ID.
    #[inline]
    pub fn with_holder_agent(mut self, agent_id: u64) -> Self {
        self.holder_agent_id = Some(agent_id);
        self
    }

    /// Sets the holder thread ID.
    #[inline]
    pub fn with_holder_thread(mut self, thread_id: u64) -> Self {
        self.holder_thread_id = Some(thread_id);
        self
    }
}

impl fmt::Display for LockTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "error=LockTimeout slot_id={} holder_agent_id={}",
            self.slot_name,
            self.holder_agent_id.map_or("none".to_string(), |id| id.to_string())
        )?;
        if let Some(thread_id) = self.holder_thread_id {
            write!(f, " holder_thread_id={}", thread_id)?;
        }
        write!(f, " timeout={:?}", self.timeout_duration)?;
        Ok(())
    }
}

impl std::error::Error for LockTimeoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<LockTimeoutError> for NexusError {
    #[inline]
    fn from(err: LockTimeoutError) -> Self {
        let msg = format!("{}", err);
        NexusError::new(ErrorKind::Timeout, msg)
            .with_context(LockTimeoutContext {
                slot_id: err.slot_id,
                slot_name: err.slot_name,
                holder_agent_id: err.holder_agent_id,
                holder_thread_id: err.holder_thread_id,
            })
    }
}

/// Context for lock timeout errors.
#[derive(Debug, Clone)]
pub struct LockTimeoutContext {
    pub slot_id: SlotId,
    pub slot_name: Cow<'static, str>,
    pub holder_agent_id: Option<u64>,
    pub holder_thread_id: Option<u64>,
}

/// Error indicating a namespace violation in typed buffer access.
///
/// This error is raised when an agent attempts to access a typed buffer
/// slot outside of its authorized namespace. This is a security violation
/// that should be logged and audited.
#[derive(Debug, Clone)]
pub struct NamespaceViolationError {
    /// The goal ID that triggered the violation.
    pub goal_id: crate::GoalId,
    /// The slot that was being accessed.
    pub slot_id: SlotId,
    /// The agent ID that attempted the access.
    pub agent_id: crate::AgentId,
    /// The operation that was attempted.
    pub operation: AccessOp,
    /// The expected namespace, if known.
    pub expected_namespace: Option<crate::NamespaceId>,
    /// The actual namespace of the agent.
    pub actual_namespace: crate::NamespaceId,
}

impl fmt::Display for NamespaceViolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "namespace violation: agent {} attempted {} on slot {} for goal {}",
            self.agent_id, self.operation, self.slot_id, self.goal_id
        )?;
        if let Some(expected) = self.expected_namespace {
            write!(f, " (expected namespace {}, was in namespace {})", expected, self.actual_namespace)?;
        }
        Ok(())
    }
}

impl std::error::Error for NamespaceViolationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

impl From<NamespaceViolationError> for NexusError {
    #[inline]
    fn from(err: NamespaceViolationError) -> Self {
        let msg = format!("{}", err);
        NexusError::new(ErrorKind::PermissionDenied, msg)
            .with_context(NamespaceViolationContext {
                goal_id: err.goal_id,
                slot_id: err.slot_id,
                agent_id: err.agent_id,
                operation: err.operation,
                actual_namespace: err.actual_namespace,
            })
    }
}

/// Context for namespace violation errors.
#[derive(Debug, Clone)]
pub struct NamespaceViolationContext {
    pub goal_id: crate::GoalId,
    pub slot_id: SlotId,
    pub agent_id: crate::AgentId,
    pub operation: AccessOp,
    pub actual_namespace: crate::NamespaceId,
}

/// Error indicating a goal dependency cycle was detected.
///
/// This error is raised when the GoalDependencyGraph detects a cycle
/// that would prevent sub-goal execution.
#[derive(Debug, Clone)]
pub struct DependencyCycleError {
    /// The goal ID where the cycle was detected.
    pub goal_id: crate::GoalId,
    /// The chain of goals forming the cycle.
    pub cycle_path: Vec<crate::GoalId>,
}

impl fmt::Display for DependencyCycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dependency cycle detected at goal {}: ", self.goal_id)?;
        for (i, goal_id) in self.cycle_path.iter().enumerate() {
            if i > 0 {
                write!(f, " -> ")?;
            }
            write!(f, "{}", goal_id)?;
        }
        Ok(())
    }
}

impl std::error::Error for DependencyCycleError {}

impl From<DependencyCycleError> for NexusError {
    #[inline]
    fn from(err: DependencyCycleError) -> Self {
        let msg = format!("{}", err);
        NexusError::new(ErrorKind::InvariantViolation, msg)
            .with_context(DependencyCycleContext {
                goal_id: err.goal_id,
                cycle_path: err.cycle_path.clone(),
            })
    }
}

/// Context for dependency cycle errors.
#[derive(Debug, Clone)]
pub struct DependencyCycleContext {
    pub goal_id: crate::GoalId,
    pub cycle_path: Vec<crate::GoalId>,
}

/// Error indicating a heartbeat timeout from the cognitive kernel.
///
/// This error is raised when the watchdog detects that the kernel
/// heartbeat has stalled beyond the acceptable threshold.
#[derive(Debug, Clone)]
pub struct HeartbeatTimeoutError {
    /// The last known heartbeat timestamp.
    pub last_heartbeat: u64,
    /// The expected heartbeat interval in milliseconds.
    pub expected_interval_ms: u64,
    /// The actual time since last heartbeat in milliseconds.
    pub elapsed_ms: u64,
    /// The threshold beyond which the kernel is considered stalled.
    pub stall_threshold_ms: u64,
}

impl fmt::Display for HeartbeatTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "heartbeat timeout: expected every {}ms, but {}ms elapsed (threshold {}ms)",
            self.expected_interval_ms, self.elapsed_ms, self.stall_threshold_ms
        )
    }
}

impl std::error::Error for HeartbeatTimeoutError {}

impl From<HeartbeatTimeoutError> for NexusError {
    #[inline]
    fn from(err: HeartbeatTimeoutError) -> Self {
        let msg = format!("{}", err);
        NexusError::new(ErrorKind::HeartbeatMissed, msg)
            .with_context(HeartbeatTimeoutContext {
                last_heartbeat: err.last_heartbeat,
                elapsed_ms: err.elapsed_ms,
            })
    }
}

/// Context for heartbeat timeout errors.
#[derive(Debug, Clone)]
pub struct HeartbeatTimeoutContext {
    pub last_heartbeat: u64,
    pub elapsed_ms: u64,
}

/// Macro for creating errors with file and line information.
///
/// # Example
///
/// ```rust
/// use nexus_utils::{nexus_err, error::ErrorKind};
///
/// let err = nexus_err!(ErrorKind::InvalidInput, "value {} is out of range", 42);
/// assert_eq!(err.message(), "value 42 is out of range");
/// ```
#[macro_export]
macro_rules! nexus_err {
    ($kind:expr, $msg:expr) => {
        $crate::error::NexusError::new($kind, $msg)
    };
    ($kind:expr, $msg:expr, $($arg:expr),+ $(,)?) => {
        $crate::error::NexusError::new($kind, format!($msg, $($arg),+))
    };
}

/// Macro for returning early with an error.
///
/// # Example
///
/// ```rust
/// use nexus_utils::error::{NexusError, NexusResult, ErrorKind};
/// use nexus_utils::bail;
///
/// fn check_positive(value: i32) -> NexusResult<()> {
///     if value < 0 {
///         bail!(ErrorKind::InvalidInput, "value must be positive, got {}", value);
///     }
///     Ok(())
/// }
///
/// let result = check_positive(-1);
/// assert!(result.is_err());
/// ```
#[macro_export]
macro_rules! bail {
    ($kind:expr, $msg:expr) => {
        return Err($crate::error::NexusError::new($kind, $msg))
    };
    ($kind:expr, $msg:expr, $($arg:expr),+ $(,)?) => {
        return Err($crate::error::NexusError::new($kind, format!($msg, $($arg),+)))
    };
}

/// Macro for asserting a condition, returning an error if false.
///
/// # Example
///
/// ```rust
/// use nexus_utils::error::{NexusResult, ErrorKind};
/// use nexus_utils::ensure;
///
/// fn divide(a: f64, b: f64) -> NexusResult<f64> {
///     ensure!(b != 0.0, ErrorKind::InvalidInput, "division by zero");
///     Ok(a / b)
/// }
///
/// let result = divide(10.0, 0.0);
/// assert!(result.is_err());
/// ```
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $kind:expr, $msg:expr) => {
        if !($cond) {
            return Err($crate::error::NexusError::new($kind, $msg));
        }
    };
    ($cond:expr, $kind:expr, $msg:expr, $($arg:expr),+ $(,)?) => {
        if !($cond) {
            return Err($crate::error::NexusError::new($kind, format!($msg, $($arg),+)));
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let err = NexusError::new(ErrorKind::InvalidInput, "test message");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert_eq!(err.message(), "test message");
    }

    #[test]
    fn test_error_chain() {
        let inner = NexusError::new(ErrorKind::Io, "file not found");
        let outer = NexusError::new(ErrorKind::Config, "failed to load")
            .with_source(inner.clone());

        assert!(outer.has_kind(ErrorKind::Io));
        assert!(outer.has_kind(ErrorKind::Config));
        assert_eq!(outer.chain().len(), 2);
    }

    #[test]
    fn test_error_kind_classification() {
        assert!(ErrorKind::Timeout.is_recoverable());
        assert!(ErrorKind::Timeout.is_transient());
        assert!(!ErrorKind::Timeout.is_fatal());

        assert!(!ErrorKind::Deadlock.is_recoverable());
        assert!(ErrorKind::Deadlock.is_fatal());

        assert_eq!(ErrorKind::InvalidInput.severity(), ErrorSeverity::Warning);
        assert_eq!(ErrorKind::InvariantViolation.severity(), ErrorSeverity::Critical);
    }

    #[test]
    fn test_result_ext() {
        fn inner() -> Result<(), std::io::Error> {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
        }

        let result: NexusResult<()> = inner().context("during config load");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.has_kind(ErrorKind::Io));
    }

    #[test]
    fn test_macros() {
        let err = nexus_err!(ErrorKind::InvalidInput, "value {} is invalid", 42);
        assert_eq!(err.message(), "value 42 is invalid");

        fn test_bail() -> NexusResult<()> {
            bail!(ErrorKind::NotFound, "resource missing");
        }
        assert!(test_bail().is_err());

        fn test_ensure() -> NexusResult<()> {
            ensure!(false, ErrorKind::InvalidState, "should be true");
            Ok(())
        }
        assert!(test_ensure().is_err());
    }

    // ============================================================================
    // Error Trait Tests
    // ============================================================================

    #[test]
    fn test_nexus_error_is_send_and_sync() {
        // Compile-time test: NexusError must be Send + Sync for cross-thread error propagation.
        // If this compiles, NexusError can be sent across threads.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NexusError>();
    }

    #[test]
    fn test_lock_timeout_formats_as_key_value() {
        // Construct LockTimeoutError with specific fields.
        // Expected: formatted string contains key=value pairs for error type, slot_id, holder_agent_id.
        use std::time::Duration;

        let err = LockTimeoutError::new("goal_states", Duration::from_millis(200))
            .with_holder_agent(4821);

        let formatted = format!("{}", err);

        // Assert each substring independently - log aggregators need parseable key=value pairs
        assert!(
            formatted.contains("error=LockTimeout"),
            "Should contain 'error=LockTimeout', got: {}",
            formatted
        );
        assert!(
            formatted.contains("slot_id=goal_states"),
            "Should contain 'slot_id=goal_states', got: {}",
            formatted
        );
        assert!(
            formatted.contains("holder_agent_id=4821"),
            "Should contain 'holder_agent_id=4821', got: {}",
            formatted
        );
    }

    #[test]
    fn test_ensure_macro_passes_when_condition_is_true() {
        // Call ensure!(true, ...) in a function that returns Result.
        // Expected: function returns Ok(()), does not bail.
        fn test_func() -> NexusResult<()> {
            ensure!(2 + 2 == 4, ErrorKind::InvalidInput, "arithmetic broken");
            Ok(())
        }

        let result = test_func();
        assert!(
            result.is_ok(),
            "ensure!(true, ...) should pass, got {:?}",
            result
        );
    }

    #[test]
    fn test_ensure_macro_fails_when_condition_is_false() {
        // Call ensure!(false, ...) in a function that returns Result.
        // Expected: function returns Err.
        fn test_func() -> NexusResult<()> {
            ensure!(false, ErrorKind::InvalidInput, "should fail");
            Ok(())
        }

        let result = test_func();
        assert!(
            result.is_err(),
            "ensure!(false, ...) should return Err, got {:?}",
            result
        );
        let err = result.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
    }
}