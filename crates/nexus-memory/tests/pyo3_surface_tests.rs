//! pyo3_surface_tests.rs — Exit gate tests for the PyO3 surface and nexus_native module.

/// import nexus_native succeeds. No ImportError. No segfault.
#[test]
fn test_import_nexus_native_succeeds() {
    todo!()
}

/// Call all 8 declared methods. Verify they return without panic.
/// dispatch_goal, register_hook, read_world_model,
/// display_pool.acquire, display_pool.release, strategy_store.get,
/// typed_buffer.read_slot, typed_buffer.write_slot.
#[test]
fn test_all_eight_methods_callable() {
    todo!()
}

/// Call dir(NexusKernel()) and verify exactly the three declared methods appear.
/// Verify no internal Rust fields are visible.
/// Verify __dict__ is absent or empty.
#[test]
fn test_dir_shows_only_declared_methods_no_rust_fields() {
    todo!()
}

/// Call a method not in the declared surface (e.g., nexus_kernel.internal_method()).
/// Verify AttributeError is raised.
/// Verify no undeclared method has leaked through.
#[test]
fn test_undeclared_methods_raise_attribute_error() {
    todo!()
}

/// Verify schema hash in mmap header matches _layout.py SCHEMA_HASH.
/// Verify NexusLayoutError is raised if they differ (not a silent wrong read).
#[test]
fn test_schema_hash_verified_at_python_startup() {
    todo!()
}

/// Run mypy against nexus_native.pyi.
/// Verify zero errors reported.
/// All 8 methods must have correct type signatures in the stub.
#[test]
fn test_mypy_zero_errors_on_nexus_native_pyi() {
    todo!()
}