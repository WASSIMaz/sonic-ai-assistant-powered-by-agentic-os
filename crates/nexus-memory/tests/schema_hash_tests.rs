//! schema_hash_tests.rs — Build-time and runtime schema hash integrity tests.

/// Verify SCHEMA_HASH constant in src/generated/layout.rs is byte-for-byte
/// identical to SCHEMA_HASH in python/nexus/_layout.py.
/// Both are read as hex strings and compared.
#[test]
fn test_schema_hash_identical_in_layout_rs_and_layout_py() {
    todo!()
}

/// Launch NEXUS Python process with the current binary but a stale _layout.py
/// (one that carries a hash from a previous build).
/// Verify Python raises NexusLayoutError with message containing both hashes.
/// Verify NEXUS does not proceed past the hash check.
/// Verify no memory is read before the abort.
#[test]
fn test_stale_layout_py_raises_nexus_layout_error() {
    todo!()
}