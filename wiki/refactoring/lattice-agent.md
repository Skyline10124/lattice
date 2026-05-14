# lattice-agent 重构建议

> Historical note: this page predates the extraction of prompt, sandbox and tools into separate crates. Treat file paths and line counts as historical leads; verify against the current workspace before implementation.

## 已发现的重构机会

### A1 — sandbox.rs 1496 行，占比最高

**文件**: `src/sandbox.rs` (1496行)

**问题**: 占整个 crate 的 17%。包含：
- SandboxConfig struct + impl (400行)
- Helper functions (150行)
- 测试函数 (800+ 行, 50+ test functions)

**建议**:
- 拆分测试到 `sandbox/tests.rs` 模块
- Helper functions (is_private_ip, path_matches_sensitive, component_matches_pattern, command_matches_allowlist) → `sandbox/helpers.rs`
- `sandbox/mod.rs` 只保留 SandboxConfig + public methods

### A2 — lib.rs 1184 行

**文件**: `src/lib.rs` (1184行)

**问题**: 包含 Agent struct + 全部方法 + PluginAgent impl + 测试代码 (400+ 行, 20+ test functions)。

**建议**:
- 拆分 Agent 核心逻辑到 `agent_core.rs` (new, builder methods, run methods)
- 拆分 PluginAgent impl 到 `plugin_agent_impl.rs`
- 拆分测试到 `tests/` 目录
- lib.rs 只保留 module declarations + pub re-exports

### A3 — tools.rs 819 行，Landlock 代码条件编译分散

**文件**: `src/tools.rs`

**问题**: `execute_bash_command` 有两份实现（Unix + non-Unix），Landlock helper 函数 (`build_landlock_read_paths`, `build_landlock_write_paths`, `apply_landlock_sandbox`) 都在 #[cfg(target_os = "linux")] 下，资源限制函数也有两个版本。

**建议**:
- Landlock 相关代码 → `sandbox/landlock.rs` (#[cfg(target_os = "linux")])
- Resource limits → `sandbox/resource_limits.rs`
- `tools.rs` 只保留 DefaultToolExecutor + execute + grep_recursive
- 条件编译更清晰

### A4 — prompt/ 模块 8 个文件但无 README/mod.rs 导引

**目录**: `src/prompt/`

**文件**: mod.rs, compiler.rs, registry.rs, system_stack.rs, memory_provider.rs, provider.rs, template.rs, types.rs

**问题**: prompt 子系统 8 个文件但缺乏模块级导引。compiler.rs 826 行是最重的。

**建议**: 在 mod.rs 中添加模块级文档注释，说明各模块职责和交互关系。

### A5 — behavior.rs 95 行但定义了跨 crate 核心类型

**文件**: `src/behavior.rs` (95行)

**问题**: BehaviorMode 和 YoloSandboxPolicy 在此定义，但被 lattice-plugin 和 lattice-bus re-export。这些类型应该是共享的，不应依赖 agent crate 的内部 module 结构。

**建议**: 参见 [B1](../boundary-analysis/issues.md#b1)。将 BehaviorMode 和 YoloSandboxPolicy 移到 lattice-core。

### A6 — is_private_ip 在 sandbox.rs 又独立实现

**文件**: `src/sandbox.rs:473-507`

**问题**: `is_private_ip` 在 lattice-core (invocation.rs, router.rs) 和 lattice-agent (sandbox.rs) 都有实现。三处相似但略有不同。

**建议**: 参见 [B5](../boundary-analysis/issues.md#b5)。lattice-core 提供 `pub fn is_private_ip`，agent 通过 crate 依赖引用。

### A7 — hook.rs 695 行，HookChain + TirithHook + VetHook

**文件**: `src/hook.rs` (695行)

**问题**: 包含 ToolHook trait + HookDenyReason + HookSeverity + HookChain struct + TirithHook + VetHook + 测试代码。

**建议**:
- TirithHook → `sandbox/hooks/tirith.rs`
- VetHook → `sandbox/hooks/vet.rs`
- ToolHook trait + HookChain → `hook.rs`（保留核心抽象）
- 测试 → `hook/tests.rs`

### A9 — SqliteMemory clone_box() panic path
**文件**: `src/memory/sqlite.rs`

**问题**: `clone_box()` uses `Box::new(*self)` which panics on OOM. `try_clone()` calls `.unwrap()` on connection reopening — will panic if SQLite file is inaccessible or locked.

**建议**: Return `Result<Self, MemoryError>` from `try_clone()`. `clone_box()` should use `try_clone()` and propagate error via `Box::new` error handling or return `Result<Box<dyn Memory>, MemoryError>`.

### A10 — SqliteMemory save_entry() silently swallows errors
**文件**: `src/memory/sqlite.rs` save_entry()

**问题**: All save failures are discarded with `.ok()` — no logging, no error propagation. Data loss without visibility.

**建议**: At minimum log the error with `tracing::warn!()`. Better: return `Result<(), MemoryError>` and let callers decide.

### A11 — AuditLog.current_size is dead code
**文件**: `src/audit.rs`

**问题**: `current_size: usize` field has `#[allow(dead_code)]`. Rotation uses filesystem metadata (`file.metadata()`), not this field. The field is misleading — suggests size-tracking exists but it's unused.

**建议**: Remove the field entirely, or implement size-based rotation using it.

### A12 — AuditLog.log_sync() drops JoinHandle
**文件**: `src/audit.rs` log_sync()

**问题**: `tokio::spawn(write_future)` returns JoinHandle but it's immediately dropped. Async write errors (filesystem full, permission denied) are silently lost.

**建议**: Store JoinHandle and check on next write, or use `tokio::spawn` + `JoinHandle::await` in a background task.

### A13 — Copy-paste poisoned lock error messages in SqliteMemory
**文件**: `src/memory/sqlite.rs`

**问题**: Poisoned lock recovery `unwrap_or_else(|e| e.into_inner())` messages reference wrong method names (e.g. "recall_like" in save_entry, "entries_by_kind" in other methods). Copy-pasted from the original method without updating.

**建议**: Use a macro or helper to generate correct method names, or remove the string formatting from the lock recovery (the actual error is the poisoned lock, not which method).

### A14 — compiler.rs assert_eq! on sections/budgets length mismatch

**文件**: `src/prompt/compiler.rs:23-27`

**问题**: `assert_eq!(sections.len(), budgets.len(), "...")` — runtime panic if caller passes mismatched lengths. Should return Result instead of panicking, especially since this is user-facing API (called during prompt compilation).

**建议**: Replace `assert_eq!` with `return Err(PromptError::MismatchedLengths)`.

### A15 — AgentRegistry silently skips failed profile loads (bus crate)

**文件**: `lattice-bus/src/registry.rs:36-42`

**问题**: Failed profile loads (`load_profile_verified` errors) are logged as `tracing::warn!` and silently skipped. Agents that fail to load disappear from the pipeline with no visibility. In production, critical agents could silently be missing.

**建议**: Add `failed_loads: Vec<(PathBuf, String)>` field to AgentRegistry for visibility, or return Result collecting all failures.

### A8 — DefaultToolExecutor 使用 reqwest::blocking::Client

**文件**: `src/tools.rs`

**问题**: VetHook 使用 `reqwest::blocking::Client` 来同步检查 curl URL。但 `DefaultToolExecutor` 的 `web_search` 工具使用 async reqwest。两个 reqwest 实例（blocking + async）共存。

**建议**: VetHook 的 URL 检查改为 async（使用 tokio runtime），统一为单一 reqwest::Client。
