//! protocols.rs — Concurrency protocol documentation per region.
//!
//! This file documents the exact concurrency protocol for each memory region
//! and how OS-specific implementation is chosen. It contains no executable code —
//! it is the authoritative reference for any engineer implementing or debugging
//! the memory subsystem.
//!
//! ═══════════════════════════════════════════════════════════════════════════
//! OS-SPECIFIC IMPLEMENTATION SELECTION
//! ═══════════════════════════════════════════════════════════════════════════
//!
//! All OS-specific code is selected at COMPILE TIME using cfg_if! blocks.
//! The public API is identical across all platforms. This is not runtime
//! detection — the binary contains only the target platform's code.
//!
//! cfg(unix) covers Linux and macOS (both use POSIX mmap + mprotect).
//! cfg(windows) covers Windows (uses VirtualAlloc + VirtualProtect).
//!
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │ Operation      │ Linux/macOS              │ Windows                 │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │ Allocate       │ mmap(MAP_SHARED|ANON)    │ CreateFileMapping +     │
//! │                │                          │ MapViewOfFile           │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │ Seal (R/O)     │ mprotect(PROT_READ)      │ VirtualProtect          │
//! │                │                          │ (PAGE_READONLY)         │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │ Unseal (R/W)   │ mprotect(PROT_READ|      │ VirtualProtect          │
//! │                │          PROT_WRITE)     │ (PAGE_READWRITE)        │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │ Free           │ munmap                   │ UnmapViewOfFile +       │
//! │                │                          │ CloseHandle             │
//! └─────────────────────────────────────────────────────────────────────┘
//!
//! ═══════════════════════════════════════════════════════════════════════
//! PYTHON ↔ RUST SHARED MEMORY PROTOCOL
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Python communicates with TypedBuffer regions via SHARED MEMORY (mmap).
//! Zero-copy: Python and Rust point their array/struct types at the same
//! physical memory pages. No serialization. No copying.
//!
//! SCHEMA HASH INTEGRITY:
//! build.rs computes SHA-256 of nexus_layout.toml → SCHEMA_HASH.
//! Rust writes SCHEMA_HASH into the 64-byte mmap header at offset 0 of
//! the APPEND_ONLY region at startup.
//! Python reads the header via ctypes at startup and calls
//! verify_schema_hash() from _layout.py.
//! If hashes differ: NexusLayoutError raised — Python refuses to proceed.
//! This makes silent layout mismatch structurally impossible.
//!
//! OBSERVATION RING (Python read path):
//! Python maps the APPEND_ONLY region via mmap module.
//! ctypes.from_buffer(mmap[OBSERVATION_RING_OFFSET + i*320 : ...])
//! interprets each slot as _layout.ObservationSnapshot.
//! No allocation. No copy. One pointer cast.
//!
//! CAPABILITY MATRIX (Python read path):
//! Python reads live_app_count from MUTABLE_REGION.live_app_count (AtomicU32).
//! np.frombuffer(mmap[MATRIX_OFFSET : MATRIX_OFFSET + live_app_count*512*4],
//!   dtype=np.float32).reshape(live_app_count, 512)
//! Creates a numpy array view with data pointer = mmap base + MATRIX_OFFSET.
//! Zero allocation confirmed by tracemalloc.
//!
//! ═══════════════════════════════════════════════════════════════════════
//! IMMUTABLE REGION PROTOCOL
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Participants: Oracle (writer), L2 Watchdog / EnvelopeMonitor (readers)
//!
//! WRITE SEQUENCE (TOCTOU-safe, enforced by StagingWriter::drop()):
//! 1. Acquire inactive staging buffer (ping-pong).
//! 2. Unseal: mprotect(staging_buffer, PROT_READ|PROT_WRITE)
//! 3. Write new WorldModelScalars into staging buffer.
//! 4. Seal: mprotect(staging_buffer, PROT_READ)    ← hardware protection
//! 5. Swap: live_ptr.store(staging_buffer, Release) ← pointer published
//! 6. Unseal old live buffer (now the new inactive staging).
//!
//! The atomic pointer is NEVER published before the seal. This eliminates
//! the TOCTOU window: no reader can observe the new pointer before hardware
//! write-protection is in place.
//!
//! READ SEQUENCE:
//! ptr = live_ptr.load(Acquire)
//! scalars = ptr as *const WorldModelScalars
//! Read any field atomically (all fields are std::sync::atomic types).
//!
//! PYTHON OBJECT RECLAMATION (ArcSwap<Py<WorldModelSnapshot>>):
//! Oracle → Python::with_gil → Py<T> (GIL released)
//! → ArcSwap::store(Arc::new(py_snapshot)) (no GIL)
//! All readers: ArcSwap::load() → Guard<Arc<Py<T>>> (no GIL)
//! Reclamation: Guard drops → Arc refcount → 0 → Py<T> drops →
//!   CPython refcount → 0 → Python object freed.
//!
//! ═══════════════════════════════════════════════════════════════════════
//! APPEND_ONLY REGION PROTOCOL
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Participants: Oracle (ring writer), Flush Thread (event log drainer),
//!               CPU Worker Pool (ring reader), detonate() (emergency writer)
//!
//! OBSERVATION RING WRITE:
//! 1. fetch_add(1, SeqCst) on write_index — claims physical slot index.
//! 2. physical = (claimed - 1) % 150
//! 3. ptr::copy_nonoverlapping(snapshot, slots[physical], 1)
//! Silent overwrite on wrap — by design (sensory memory, 30s window).
//!
//! RING READ (SeqLock protocol):
//! 1. pre_index = write_index.load(SeqCst)
//! 2. Read slot via memcpy.
//! 3. post_index = write_index.load(SeqCst)
//! 4. If post_index - pre_index >= 150: ring lapped reader, RETRY.
//!    Max retries: 3. After 3 failures: return ReadResult::RingLapped.
//!
//! EVENT LOG WRITE (ACCESS events):
//! 1. fetch_add(size_of::<EventRecord>(), SeqCst) on write_index — claims bytes.
//! 2. ptr::copy_nonoverlapping(record, base + claimed_offset, 1)
//! ~10ns. No lock. No SQLite.
//!
//! EVENT LOG FLUSH (every 500ms):
//! if !sqlite_ready: skip. Ring accumulates. Nothing dropped.
//! drain() → batch INSERT into security_events (WAL mode).
//!
//! ═══════════════════════════════════════════════════════════════════════
//! MUTABLE REGION PROTOCOL
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Participants: Any agent with slot in its namespace (readers and writers)
//!
//! WRITE SEQUENCE:
//! 1. holder_agent_id.store(agent_id, Release)
//! 2. guard = inner.try_write_for(Duration::from_millis(500))
//! 3. On None (timeout): holder_agent_id.store(0, Release)
//!    Return LockTimeoutError{slot_id, holder_agent_id, waited_ms: 500}
//!    Caller MUST publish LOCK_TIMEOUT to Bus.
//! 4. On Some(guard): run f(&mut T)
//! 5. new_lamport = global_lamport.advance_to(global_lamport.load() + 1)
//! 6. last_modified_lamport.store(new_lamport, Release)
//! 7. holder_agent_id.store(0, Release)
//! 8. drop(guard) — releases the write lock.
//!
//! READ SEQUENCE:
//! inner.read() — blocking, no timeout (writes always release within 500ms).
//! Run f(&T). Drop guard.
//!
//! ═══════════════════════════════════════════════════════════════════════
//! COMPUTE REGION PROTOCOL
//! ═══════════════════════════════════════════════════════════════════════
//!
//! Participants: CPU Worker Pool (sole writer), Contextualizers (readers)
//!
//! WRITE EXCLUSIVITY (structural, not a lock):
//! COMPUTE_WRITE is not in any agent's CapabilityToken namespace.
//! Only the CPU Worker Pool's token has compute_write = true.
//! TypedBufferHandle::write_slot() checks token.has_compute_write() before
//! allowing access to COMPUTE region slots.
//! An agent attempting COMPUTE write receives NamespaceViolationSentinel.
//!
//! WRITE SEQUENCE (CPU Worker Pool, every 5 minutes):
//! 1. Publish COMPUTE_WRITE_BEGIN to Bus.
//! 2. begin_matrix_write() → MatrixWriter (sets write_in_progress = true).
//! 3. MatrixWriter::write_rows() — ptr::copy_nonoverlapping, 2MB in < 1ms.
//! 4. Atomically write live_app_count to MUTABLE_REGION.live_app_count.
//! 5. drop(MatrixWriter) — sets write_in_progress = false.
//! 6. Publish COMPUTE_WRITE_DONE to Bus.
//! live_app_count is written BEFORE COMPUTE_WRITE_DONE is published.
//! This guarantees no Contextualizer reads the new count before the matrix is complete.
//!
//! READ SEQUENCE (Contextualizer):
//! 1. Check Bus: if COMPUTE_WRITE_BEGIN received, wait for COMPUTE_WRITE_DONE.
//! 2. live_app_count = MUTABLE_REGION.live_app_count.load(Acquire)
//! 3. matrix_slice(live_app_count) — returns (ptr, num_rows, dim) or None.
//! 4. np.frombuffer(ptr, dtype=float32).reshape(num_rows, dim) — zero copy.
//! 5. dot_product(matrix, intent_embedding) — BLAS, sub-millisecond.