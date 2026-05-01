# LATTICE Project Status

**Updated**: 2026-04-30
**Focus**: Runtime hang fix for Agent.send_message()

## This Sprint: Nested Runtime Hang Fix

### Problem
`Agent.send_message()` and related sync methods called `SHARED_RUNTIME.block_on()` directly.
When invoked from inside a tokio runtime context (e.g., `#[tokio::main]`), `block_on()` on
a different runtime causes thread deadlock: the calling thread blocks, the outer runtime
can't make progress, and reqwest HTTP requests stall indefinitely.

### Root Cause (5-Why RCA)
1. **Why hang?** → `SHARED_RUNTIME.block_on()` called from thread already inside a runtime
2. **Why block_on fails?** → Nested runtime: outer `#[tokio::main]` already owns the thread, inner block_on needs it too
3. **Why need same thread?** → `current_thread` runtime has 1 thread; blocking it freezes IO driver
4. **Why no detection?** → Agent sync API had no mechanism to detect/reuse caller's runtime
5. **Root cause** → Sync methods directly called `block_on()` without handling "already inside runtime" scenario

### Fix
Added `run_async()` helper in `lattice-agent/src/lib.rs`:
- **Outside any runtime**: `SHARED_RUNTIME.block_on(future)` directly (fast path)
- **Inside a runtime**: `Handle::try_current()` → `spawn_blocking()` + `SHARED_RUNTIME.block_on()` + `mpsc::channel`. The blocking thread pool runs outside any runtime context, so `block_on()` is safe. Works on both `current_thread` and `multi_thread` runtimes.

Additional changes:
- `Agent.memory` field: `Box<dyn Memory>` → `Arc<dyn Memory>` (enables `Send + 'static` for async memory saves)
- `Memory` trait: added `clone_arc()` method (complements existing `clone_box()`)
- `chat_with_retry()`: clones resolved/messages/tools before passing to `run_async()` (removes `&self` borrows)
- `run_chat()`: all mutable state moved inside `async move` block, returns tuple; self mutations deferred to after async completion
- `with_memory()` signature: `Box<dyn Memory>` → `Arc<dyn Memory>`
- Harness callers: `clone_box()` → `clone_arc()`

### Files Modified
- `lattice-agent/src/lib.rs` — run_async() helper, all 4 block_on() calls replaced, memory Arc refactor
- `lattice-memory/src/lib.rs` — added `clone_arc()` to Memory trait
- `lattice-harness/src/pipeline.rs` — `clone_box()` → `clone_arc()`
- `lattice-harness/src/dispatch.rs` — `clone_box()` → `clone_arc()`

### Test Results
- lattice-agent: 3/3 passed
- lattice-memory: 6/6 passed
- lattice-token-pool: 3/3 passed
- lattice-core (excluding pre-existing router mutex failures): 155/155 passed

### Pre-existing Issues (NOT from this fix)
- `router::tests` — 8 tests fail due to global Mutex poisoning (PoisonError cascade from one panic). These are a pre-existing concurrency bug in the router's credential cache, unrelated to this fix.

## Remaining Work
- P0-1: Python API only exposes resolver, no chat/streaming
- P1-4: ErrorClassifier not wired through streaming phase
- W-1: lattice-cli compilation failures (16 E0583 errors)
- W-2: lattice-tui directory missing (but now exists with skeleton)
- Router Mutex poisoning (newly surfaced)