# NEXUS — AI Assistant Powered by an Agentic OS

> **Where goals replace processes. Where agents share one intelligence.**

NEXUS is not a chatbot. It is an **agentic operating system** — a new kind of AI assistant that observes your environment, decomposes complex goals into sub-tasks, dispatches specialized agents, and completes work autonomously on your behalf. Built on a Rust cognitive kernel with a Python agent space, NEXUS treats your machine the way an OS treats hardware: as a resource to be managed, not a canvas to be clicked.

---

## What Makes NEXUS Different

| Traditional AI Assistant | NEXUS |
|--------------------------|-------|
| Responds to prompts | Pursues goals |
| Forgets context between messages | Maintains persistent episodic memory |
| Single model, single thread | Fleet of specialized agents |
| You operate the tools | NEXUS operates the tools |
| Stateless | Stateful — observes your machine every 200ms |

---

## Architecture at a Glance

```
┌─────────────────────────────────────────────────────┐
│                   Agent Space (Python)               │
│  Planner · Contextualizer · ProcessOrchestrator      │
│  UI-TARS · Specialist Fleet · LLM Gateway            │
├─────────────────────────────────────────────────────┤
│              Cognitive Kernel (Rust)                 │
│  Goal Scheduler · Dependency Graph · Event Bus       │
│  Envelope Monitor · Fleet Monitor · Watchdogs        │
├─────────────────────────────────────────────────────┤
│              TypedBuffer Memory Regions              │
│  IMMUTABLE · APPEND_ONLY · MUTABLE · COMPUTE         │
│  mmap shared memory · hardware-enforced namespaces   │
├─────────────────────────────────────────────────────┤
│                Security Kernel (Python)              │
│  Capability Broker · Namespace Enforcement           │
│  Provenance Tracking · Audit Log                     │
└─────────────────────────────────────────────────────┘
```

---

## Key Properties

- **Goals, not commands** — you describe what you want, NEXUS figures out how
- **200ms observation cycle** — the Oracle polls your machine state continuously, feeding a live WorldModel to all agents
- **Hardware-enforced memory isolation** — each agent accesses only its authorized memory slots via `mprotect(PROT_READ)` and capability tokens in Rust thread-local storage
- **Prompt injection resistant** — the Security Kernel validates every action before execution; compromised agents cannot escalate their namespace
- **Zero-copy Python/Rust bridge** — agents read the capability matrix and observation ring via `np.frombuffer()` directly from mmap — no serialization, no copying
- **Production-grade concurrency** — lock-free ring buffers, ArcSwap for WorldModel pointer swaps, parking_lot RwLock with 500ms write timeouts on every mutable slot
- **Persistent episodic memory** — the Chronicler appends daily markdown journals; the Consolidator builds long-term world models from consequence records

---

## Repository Structure

```
nexus/
├── crates/
│   ├── nexus-utils/          ← shared concurrency primitives (Rust)
│   │   ├── atomic.rs         ← AtomicCounter, AtomicInt, Lamport clock
│   │   ├── rwlock.rs         ← TimedRwLock — SP-1 stall path fix
│   │   ├── ring_buffer.rs    ← lock-free MPMC ring buffer
│   │   ├── timing.rs         ← DriftCorrectedTimer, HeartbeatTimer
│   │   └── error.rs          ← structured key=value error types
│   │
│   └── nexus-memory/         ← TypedBuffer regions + hot-tier store (Rust + PyO3)
│       ├── regions/          ← IMMUTABLE · APPEND_ONLY · MUTABLE · COMPUTE
│       ├── slots/            ← typed slot structs (#[repr(C)])
│       ├── namespace/        ← CapabilityToken TLS + violation sentinel
│       ├── allocator/        ← cross-platform mmap + slab allocator
│       └── hot_store/        ← WorldModel ArcSwap + embedding model
│
├── python/
│   └── nexus/
│       ├── kernel/           ← goal scheduler, dependency graph, bus
│       ├── agents/           ← planner, contextualizer, specialist fleet
│       ├── security/         ← capability broker, provenance, audit
│       ├── oracle/           ← environment observation (200ms cycle)
│       └── memory/           ← episodic memory, consolidator, world builder
│
├── nexus_layout.toml         ← single source of truth for memory layout
└── nexus_native.pyi          ← PyO3 surface type stubs (mypy verified)
```

---

## Prerequisites

| Dependency | Version | Notes |
|------------|---------|-------|
| Rust | ≥ 1.75 | stable toolchain (`rustup default stable`) |
| Python | ≥ 3.11 | |
| maturin | ≥ 1.4 | builds the PyO3 extension |
| numpy | ≥ 1.26 | zero-copy matrix bridge |
| SQLite | ≥ 3.35 | WAL mode required |

**Windows:** requires MSVC toolchain (`stable-x86_64-pc-windows-msvc`). Install Visual Studio Build Tools with "Desktop development with C++".

**Linux / macOS:** standard Rust + Python toolchain. No additional system dependencies.

---

## Getting Started

### 1. Clone the repository

```bash
git clone https://github.com/your-username/nexus.git
cd nexus
```

### 2. Set up the Rust toolchain

```bash
# Windows
rustup default stable-x86_64-pc-windows-msvc

# Linux / macOS
rustup default stable
```

### 3. Build the Rust crates and generate the PyO3 extension

```bash
cd crates/nexus-utils
cargo test

cd ../nexus-memory
cargo build
```

This step runs `build.rs` which generates two files automatically:
- `src/generated/layout.rs` — Rust memory layout constants
- `python/nexus/_layout.py` — Python ctypes structure definitions

Both files contain an identical `SCHEMA_HASH`. If they ever differ, startup aborts with `NexusLayoutError`.

### 4. Install Python dependencies

```bash
pip install maturin numpy
```

### 5. Build and install the nexus_native Python extension

```bash
cd crates/nexus-memory
maturin develop
```

### 6. Verify the installation

```bash
python -c "import nexus_native; print('nexus_native OK')"
```

### 7. Run the test suite

```bash
# Rust unit and integration tests
cargo test --workspace

# Python type checking
mypy nexus_native.pyi
```

### 8. Launch NEXUS

```bash
python -m nexus.kernel.boot
```

On first launch NEXUS will:
1. Initialize `nexus.db` in WAL mode
2. Map all four memory regions (~330MB total mmap)
3. Load the resident embedding model (CPU fallback if no GPU detected)
4. Start the Oracle observation cycle (200ms polls)
5. Declare **NEXUS ready** — goals can now be accepted

---

## Current Build Status

| Component | Status |
|-----------|--------|
| nexus-utils | ✅ 65 unit tests + 35 doc tests passing |
| nexus-memory regions | 🔧 In progress (Phase 2) |
| Cognitive Kernel | 🔧 Planned (Phase 3) |
| Agent Fleet | 🔧 Planned (Phase 4–6) |
| LLM Gateway | 🔧 Planned (Phase 7) |
| Learning Subsystem | 🔧 Planned (Phase 8) |

---

## Sacred Rules

NEXUS operates under hard architectural constraints that cannot be overridden:

1. **Never inject steps into UI-TARS mid-execution** — once a display action sequence begins, it runs to completion
2. **Every action is gated** — the Security Kernel validates before execution, not after
3. **No agent reads another agent's namespace** — hardware-enforced via mprotect + capability tokens
4. **No goal is accepted without SQLite ready** — auditability is a boot prerequisite, not a feature

---

## Contributing

This project is in active development. Before contributing:

1. Read `nexus_layout.toml` — all memory constants are locked here
2. Run `cargo test --workspace` — all tests must pass before any PR
3. Run `mypy nexus_native.pyi` — zero type errors required
4. Do not modify `src/generated/layout.rs` or `python/nexus/_layout.py` — these are generated by `build.rs`

---

## License

MIT OR Apache-2.0
