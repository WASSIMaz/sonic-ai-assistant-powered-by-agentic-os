//! namespace — Namespace enforcement and access logging.
//!
//! This module provides:
//! - `violation_sentinel`: PyO3 sentinel class with 10 dunder overrides
//! - `access_logger`: Two-tier ACCESS/USE event logging

pub mod access_logger;
pub mod violation_sentinel;

pub use violation_sentinel::{CapabilityToken, NamespaceSet, NamespaceViolationSentinel};
