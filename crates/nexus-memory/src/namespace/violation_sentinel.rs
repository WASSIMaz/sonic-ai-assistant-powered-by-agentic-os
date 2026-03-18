//! violation_sentinel.rs — Hardware-enforced namespace violation sentinel.
//!
//! DESIGN (from Q11):
//! The sentinel is a #[pyclass(frozen)] Rust struct registered with CPython's
//! slot table via #[pymethods]. This is NOT a pure Python class.
//!
//! Why PyO3 #[pymethods] is required (not pure Python):
//! 1. CPython looks up special methods (tp_as_number, tp_as_sequence etc.) on
//!    the TYPE, not the instance. A pure Python class can define __bool__ as an
//!    instance method but CPython may bypass it. PyO3 writes directly into
//!    CPython's C-level slot table at type registration time.
//! 2. Pure Python classes are monkey-patchable: an agent under prompt injection
//!    could replace NamespaceViolationSentinel.__bool__ = lambda self: True.
//!    PyO3 #[pymethods] slots are set in C at registration time — not in Python's
//!    mutable __dict__ — and cannot be patched from Python.
//!
//! TEN DUNDERS OVERRIDDEN (every one calls detonate()):
//! __getattr__, __getitem__, __bool__, __iter__, __len__,
//! __eq__, __str__, __repr__, __contains__, __call__
//!
//! DETONATION SEQUENCE (inside detonate()):
//! 1. Acquire Python traceback via Python::with_gil + traceback module.
//! 2. Write USE event SYNCHRONOUSLY to SQLite inside detonate().
//!    Cost: 1-5ms. Acceptable — agent is already in a crash path.
//!    If SQLite not ready: write to emergency buffer with promotion_pending=1.
//! 3. Raise NamespaceViolationUsed exception with full context message:
//!    "slot_id={} agent_id={} lamport_ts={} operation={} traceback=..."
//!    Exception message is self-contained — no subsequent lookup needed.
//!
//! CAPABILITY TOKEN THREAD-LOCAL (from Q13):
//! CURRENT_TOKEN is a Rust thread_local! — NOT exported through PyO3 API.
//! The Kernel sets it once per agent thread at spawn time.
//! read_slot() and write_slot() perform TLS lookup (~1ns) + frozenset check (O(1)).
//! An agent has NO path to forge, read, or overwrite CURRENT_TOKEN from Python.

use std::sync::Arc;
use std::collections::HashSet;
use std::cell::RefCell;
use std::sync::atomic::AtomicU64;
use pyo3::prelude::*;
use pyo3::exceptions::PyException;
use nexus_utils::AgentId;
use nexus_utils::error::SlotId;

/// The set of slot IDs an agent is authorized to access.
/// Immutable after construction — Arc ensures no copy on each TLS lookup.
pub type NamespaceSet = Arc<HashSet<SlotId>>;

/// A capability token issued by the Kernel at agent thread spawn.
/// Lives in thread-local storage. Not exported to Python.
/// Not constructible from Python (no #[new] in #[pymethods]).
#[derive(Clone)]
pub struct CapabilityToken {
    /// The agent this token was issued to.
    pub agent_id: AgentId,

    /// The immutable set of SlotIds this agent is authorized to access.
    /// Arc<HashSet<SlotId>> — shared across reads without copying.
    pub namespace: NamespaceSet,

    /// Whether this token includes COMPUTE_WRITE authorization.
    /// Only the CPU Worker Pool's token has this set to true.
    pub compute_write: bool,

    /// Lamport timestamp at which this token was issued.
    /// For audit trails and happened-before reconstruction.
    pub lamport_issued: u64,
}

impl CapabilityToken {
    /// Set the current thread's CapabilityToken.
    ///
    /// Called ONCE per agent thread by the Kernel at spawn time.
    /// Not exported through PyO3 API — no Python path to this function.
    pub fn set(token: CapabilityToken) {
        todo!()
    }

    /// Get the current thread's CapabilityToken.
    ///
    /// Returns None if this thread has no token (called from a non-agent thread).
    /// Not exported through PyO3 API.
    pub fn get() -> Option<CapabilityToken> {
        todo!()
    }

    /// Check if this token authorizes access to the given slot.
    /// O(1) hash set lookup.
    #[inline]
    pub fn allows(&self, slot_id: SlotId) -> bool {
        todo!()
    }

    /// Check if this token includes COMPUTE_WRITE authorization.
    #[inline]
    pub fn has_compute_write(&self) -> bool {
        todo!()
    }
}

// Thread-local storage for the current agent's CapabilityToken.
// NOT accessible from Python — this is a Rust thread_local! macro.
// The agent module running in Python has no path to read or write this.
thread_local! {
    static CURRENT_TOKEN: RefCell<Option<CapabilityToken>> = RefCell::new(None);
}

/// The opaque namespace violation sentinel returned when an agent accesses a
/// slot outside its CapabilityToken namespace.
///
/// Every Python operation on this object (getattr, getitem, bool, iter, len,
/// eq, str, repr, contains, call) triggers detonation:
///   1. USE event written synchronously to SQLite.
///   2. NamespaceViolationUsed exception raised with full context.
///
/// The sentinel is #[pyclass(frozen)] — its fields are not accessible from Python.
/// It is constructible only by Rust (no #[new] on the PyO3 side).
#[pyclass(frozen, name = "NamespaceViolationSentinel")]
pub struct NamespaceViolationSentinel {
    /// The slot the agent attempted to access.
    slot_id: SlotId,

    /// The agent that made the access attempt.
    agent_id: AgentId,

    /// Lamport timestamp at the moment of the violation.
    lamport_ts: u64,

    /// The operation attempted: "read", "write", "append", or "compute".
    operation: String,
}

#[pymethods]
impl NamespaceViolationSentinel {
    /// All dunders call detonate() — no operation on this object is safe.
    /// PyO3 hooks these directly into CPython's C-level slot table.
    /// They cannot be monkey-patched from Python.

    fn __getattr__(&self, py: Python<'_>, _name: &str) -> PyResult<PyObject> {
        self.detonate(py)
    }

    fn __getitem__(&self, py: Python<'_>, _key: PyObject) -> PyResult<PyObject> {
        self.detonate(py)
    }

    fn __bool__(&self, py: Python<'_>) -> PyResult<bool> {
        self.detonate(py)?;
        unreachable!("detonate() always raises NamespaceViolationUsed")
    }

    fn __iter__(&self, py: Python<'_>) -> PyResult<PyObject> {
        self.detonate(py)
    }

    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        self.detonate(py)?;
        unreachable!()
    }

    fn __eq__(&self, py: Python<'_>, _other: PyObject) -> PyResult<bool> {
        self.detonate(py)?;
        unreachable!()
    }

    fn __str__(&self, py: Python<'_>) -> PyResult<String> {
        self.detonate(py)?;
        unreachable!()
    }

    fn __repr__(&self, py: Python<'_>) -> PyResult<String> {
        self.detonate(py)?;
        unreachable!()
    }

    fn __contains__(&self, py: Python<'_>, _item: PyObject) -> PyResult<bool> {
        self.detonate(py)?;
        unreachable!()
    }

    fn __call__(
        &self,
        py: Python<'_>,
        _args: &pyo3::types::PyTuple,
        _kwargs: Option<&pyo3::types::PyDict>,
    ) -> PyResult<PyObject> {
        self.detonate(py)
    }
}

impl NamespaceViolationSentinel {
    /// Detonation sequence:
    /// 1. Acquire Python traceback via Python::with_gil + traceback.format_stack().
    /// 2. Write USE event synchronously to SQLite (1-5ms, acceptable on crash path).
    ///    If SQLite not ready: write to APPEND_ONLY emergency buffer with promotion_pending=1.
    /// 3. Raise NamespaceViolationUsed with message:
    ///    "slot_id={slot_id} agent_id={agent_id} lamport_ts={lamport_ts} operation={op}"
    ///    Message is self-contained — no post-hoc lookup needed for forensics.
    fn detonate(&self, py: Python<'_>) -> PyResult<PyObject> {
        todo!()
    }
}

/// The exception raised by NamespaceViolationSentinel::detonate().
/// Contains the full violation context in its message string.
pyo3::create_exception!(
    nexus_native,
    NamespaceViolationUsed,
    pyo3::exceptions::PyException,
    "Raised when a NamespaceViolationSentinel is dereferenced. \
     Contains slot_id, agent_id, lamport_ts, and traceback in the message."
);