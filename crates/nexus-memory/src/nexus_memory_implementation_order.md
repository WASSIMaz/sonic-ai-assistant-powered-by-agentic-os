# NEXUS-MEMORY Phase 2 — Implementation Order

> **Rule:** Each file depends on everything above it being compilable first.
> Do not proceed to the next layer until the stop condition for the current layer passes.

---

## Layer 0 — Generated (run `cargo build` first, touch no source yet)

| # | File | Purpose |
|---|------|---------|
| 1 | `nexus_layout.toml` | already built required for `build.rs`. |
| 2 | `build.rs` | Reads toml, computes SCHEMA_HASH, emits `layout.rs` and `_layout.py`. |
| 3 | `src/generated/layout.rs` | **Emitted by build.rs — do not edit.** All Rust constants. |
| 4 | `python/nexus/_layout.py` | **Emitted by build.rs — do not edit.** Python ctypes.Structure definitions. |

**Stop condition:** `cargo build` succeeds. Both generated files exist. `SCHEMA_HASH` is byte-for-byte identical in both files.

---

## Layer 1 — Errors and Protocols (no dependencies on anything in this crate)

| # | File | Purpose |
|---|------|---------|
| 5 | `src/error.rs` | All error types: `MmapError`, `SlabExhausted`, `LockTimeoutError`, `NamespaceViolationError`, `BootError`, `BufferError`. No deps on other nexus-memory files. |
| 6 | `src/protocols.rs` | Documentation only. Concurrency protocol per region. No executable code. Compiles trivially. |

**Stop condition:** `cargo build` succeeds with no errors.

---

## Layer 2 — OS Abstraction (depends only on `error.rs`)

| # | File | Purpose |
|---|------|---------|
| 7 | `src/allocator/mod.rs` | `pub mod` declarations only. |
| 8 | `src/allocator/mmap.rs` | Cross-platform anonymous shared memory. `mmap`/`VirtualProtect` abstraction. `seal_read_only()` and `unseal()`. |

**Stop condition:** `cargo build` succeeds. Unit tests in `mmap.rs` pass:
```
cargo test allocator
```

---

## Layer 3 — Slab Allocator (depends on `mmap.rs` and `generated/layout.rs`)

| # | File | Purpose |
|---|------|---------|
| 9 | `src/allocator/slab.rs` | Static pre-allocation. Lock-free free-list. Four slab classes: `GoalContractSlab`, `GoalStateSlab`, `ObservationSlab`, `AtomicCounterSlab`. |

**Stop condition:** All 4 unit tests pass:
```
cargo test allocator::slab
```

---

## Layer 4 — Slot Structs (depend only on `error.rs` and `generated/layout.rs`)

Pure data definitions. Implement in this exact order — later slots reference earlier ones.

| # | File | Purpose |
|---|------|---------|
| 10 | `src/slots/mod.rs` | `pub mod` declarations only. |
| 11 | `src/slots/world_model.rs` | `WorldModelScalars` `#[repr(C)]` fast-path struct. Scalar fields read by L2 Watchdog and EnvelopeMonitor without GIL. |
| 12 | `src/slots/observation_ring.rs` | `ObservationSnapshot` `#[repr(C)]` 320-byte struct + `ObservationRing`. SeqLock read protocol. Compile-time 320-byte size assert. |
| 13 | `src/slots/event_log_buffer.rs` | `EventRecord` `#[repr(C)]` + `EventLogBuffer`. Two-tier ACCESS/USE logging. Emergency buffer. |
| 14 | `src/slots/goal_states.rs` | `GoalStateEntry` — Slab B. GoalDependencyGraph entries. 1024 bytes per slot. |
| 15 | `src/slots/agent_registry.rs` | `AgentRegistryEntry` — heartbeat `AtomicU64`, status `AtomicU8`. 512 bytes per slot. |
| 16 | `src/slots/token_state.rs` | `TokenStateEntry` — Input Surface Token atomic CAS acquire/release. 64 bytes per slot. |
| 17 | `src/slots/surface_assignments.rs` | `SurfaceAssignmentEntry` — `surface_id → agent_id` mapping. 64 bytes per slot. |
| 18 | `src/slots/capability_map.rs` | `CapabilityMapEntry` — `task_sig → [resource_id]` hot-reloadable routing. 1024 bytes per slot. |
| 19 | `src/slots/token_debt_ledger.rs` | `TokenDebtEntry` — `cumulative_debt`, `call_count`, `avg_overrun`. 256 bytes per slot. |
| 20 | `src/slots/machine_profile.rs` | Frozen `MachineProfile`. Reference to capability matrix in `ComputeRegion`. |
| 21 | `src/slots/hnsw_index.rs` | `usearch::Index` wrapper. In-memory ANN index. Atomic pointer swap on rebuild. |
| 22 | `src/slots/embedding_cache.rs` | `DashMap<u128, CacheEntry>` keyed by `xxh3_128`. LRU eviction at 1024 entries. `lru::LruCache` eviction index inside `Mutex`. |

**Stop condition:** `cargo build` succeeds. `ObservationSnapshot` size assert passes (320 bytes verified at compile time).

---

## Layer 5 — Namespace Enforcement (depends on `slots/event_log_buffer.rs` and `error.rs`)

> Namespace must exist **before** regions because every region access calls the namespace check.

| # | File | Purpose |
|---|------|---------|
| 23 | `src/namespace/mod.rs` | `pub mod` declarations only. |
| 24 | `src/namespace/violation_sentinel.rs` | `CapabilityToken` in `thread_local!` TLS (not exported to Python). `NamespaceViolationSentinel` `#[pyclass(frozen)]` with 10 dunder overrides. All dunders call `detonate()`. |
| 25 | `src/namespace/access_logger.rs` | Two-tier logging. ACCESS → `EventLogBuffer` (~10ns, non-blocking). USE → synchronous SQLite inside `detonate()` (1–5ms). Emergency buffer path for pre-SQLite USE events. |

**Stop condition:** `cargo build` succeeds. PyO3 sentinel class registers without panic. TLS token is inaccessible from Python (verified manually).

---

## Layer 6 — Memory Regions (depend on allocator, slots, namespace, error)

> Implement in dependency order. APPEND_ONLY before MUTABLE because MUTABLE's flush thread writes to SQLite which APPEND_ONLY initializes.

| # | File | Purpose |
|---|------|---------|
| 26 | `src/regions/mod.rs` | `pub mod` declarations only. |
| 27 | `src/regions/immutable_region.rs` | Ping-pong mmap. `mprotect`/`VirtualProtect` seal. `StagingWriter::drop()` enforces TOCTOU-safe sequence: seal → swap → unseal_old. `ArcSwap<Py<WorldModelSnapshot>>` for full Python dataclass. |
| 28 | `src/regions/append_only_region.rs` | 2MB mmap. `ObservationRing` at offset 64. `EventLogBuffer` at offset 48064. Emergency buffer at offset 2031616. 500ms flush thread. |
| 29 | `src/regions/mutable_region.rs` | 8MB mmap. `MutableSlot<T>` with `parking_lot::RwLock` per slot. `try_write_for(500ms)`. Lamport clock advanced on every successful write. |
| 30 | `src/regions/compute_region.rs` | 256MB mmap. No locks. Write exclusivity via `CapabilityToken` namespace. `MatrixWriter` for float32 matrix writes. `write_in_progress: AtomicBool` for Bus coordination. |

**Stop condition:** Each region's own unit tests pass before moving to the next region.
```
cargo test regions::immutable
cargo test regions::append_only
cargo test regions::mutable
cargo test regions::compute
```

---

## Layer 7 — Hot Store (depends on regions, slots, PyO3)

| # | File | Purpose |
|---|------|---------|
| 31 | `src/hot_store/mod.rs` | `pub mod` declarations only. |
| 32 | `src/hot_store/world_model_store.rs` | `ArcSwap<Py<WorldModelSnapshot>>`. GIL held only during Python object construction. `store()` and `load()` both GIL-free. Arc reclamation chain. |
| 33 | `src/hot_store/machine_profile_store.rs` | Frozen `MachineProfile`. 5-minute background refresh cycle. Triggers capability matrix write in `ComputeRegion`. |
| 34 | `src/hot_store/resident_embedding_model.rs` | Single model instance. Backend detection at startup: CUDA → CoreML → ONNX(gpu) → ONNX(cpu) → CPU. CPU fallback always available. |
| 35 | `src/hot_store/curated_facts_store.rs` | `MEMORY.md` loaded into frozen no-decay struct. Never evicted. |
| 36 | `src/hot_store/protocols.rs` | Hot-store access protocol documentation. No executable code. |

**Stop condition:** `cargo build` succeeds. `WorldModelStore` ArcSwap load test passes:
```
cargo test hot_store::world_model_store
```

---

## Layer 8 — Crate Root and PyO3 Module (depends on everything above)

| # | File | Purpose |
|---|------|---------|
| 37 | `src/lib.rs` | `NexusMemory::init()` 9-step boot sequence. `nexus_native` PyO3 module. Exactly 8 declared methods across 4 classes: `NexusKernel`, `DisplayPool`, `StrategyStore`, `TypedBufferHandle`. |

**Stop condition:** `cargo build` succeeds. Python import succeeds:
```bash
python -c "import nexus_native"
```

---

## Layer 9 — Python Files (after `lib.rs` compiles)

| # | File | Purpose |
|---|------|---------|
| 38 | `python/nexus/exceptions.py` | `NexusLayoutError`, `NamespaceViolationUsed`. |
| 39 | `python/nexus/__init__.py` | Imports `_layout`. Calls `verify_schema_hash()` on import. Raises `NexusLayoutError` on hash mismatch. |
| 40 | `nexus_native.pyi` | Type stubs for all 8 methods. Verified by mypy. |

**Stop condition:**
```bash
python -c "import nexus_native"   # no error
mypy nexus_native.pyi              # zero errors
```

---

## Layer 10 — Tests (after all source files compile)

Write and run tests in the same order as the components they cover.

| # | File | Covers |
|---|------|--------|
| 41 | `tests/schema_hash_tests.rs` | Build-time hash codegen pipeline. Stale `_layout.py` detection. |
| 42 | `tests/immutable_region_tests.rs` | TOCTOU safety. ArcSwap reclamation. Hardware fault on sealed write. GIL sequencing. |
| 43 | `tests/append_only_tests.rs` | Ring wrap. SeqLock retry. Emergency buffer promotion. 500ms flush. |
| 44 | `tests/mutable_region_tests.rs` | Lamport advancement. Write timeout. Concurrent writes. |
| 45 | `tests/compute_region_tests.rs` | Matrix byte identity. Namespace violation on write. Bus coordination. |
| 46 | `tests/namespace_tests.rs` | Sentinel return. Detonation. Token unforgeability. USE event durability. |
| 47 | `tests/boot_sequence_tests.rs` | Clean boot. Corrupted SQLite hard stop. Schema hash mismatch abort. |
| 48 | `tests/pyo3_surface_tests.rs` | Import. 8 methods callable. dir() boundary. mypy zero errors. |
| 49 | `tests/performance_tests.rs` | Event log < 15ns. TLS check < 5ns. frombuffer < 1ms. ArcSwap < 1ms. Peak footprint. |
| 50 | `tests/red_team_tests.rs` | Object graph traversal blocked. Namespace expansion via introspection blocked. |

**Final stop condition:**
```bash
cargo test   # all 39 named tests pass, zero failures
mypy nexus_native.pyi   # zero errors
```

---

## Summary Table

| Layer | Files | Depends On |
|-------|-------|------------|
| 0 — Generated | 1–4 | Nothing (build.rs reads toml) |
| 1 — Errors | 5–6 | Nothing |
| 2 — OS Abstraction | 7–8 | Layer 1 |
| 3 — Slab | 9 | Layers 1–2 |
| 4 — Slot Structs | 10–22 | Layers 1, 0 |
| 5 — Namespace | 23–25 | Layers 1, 4 |
| 6 — Regions | 26–30 | Layers 1–5 |
| 7 — Hot Store | 31–36 | Layers 1–6 |
| 8 — Crate Root | 37 | Layers 1–7 |
| 9 — Python | 38–40 | Layer 8 |
| 10 — Tests | 41–50 | Layers 1–9 |

**Total files: 50. Total named tests: 39. Phase 2 complete when all 39 pass.**
