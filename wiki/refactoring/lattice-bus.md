# lattice-bus 重构建议

> Historical note: `lattice-bus` is now represented by `lattice-orchestrator`. The items below are older scan notes and must be re-verified against `crates/lattice-orchestrator/` before implementation.

## 已发现的重构机会

### B1 — BusConfig / BusToml 默认值不一致 (P0 Bug)

**文件**: `src/lib.rs:147-152` vs `src/lattice_dir.rs:12-22`

**问题**: 
- BusConfig::default().max_concurrent_calls = **32**
- BusToml::default().max_concurrent_calls = **1**
- 当 BusToml::to_bus_config() 转换时用 TOML default (1)
- 当 BusConfig::default() 直接用（如 InMemoryBus::with_defaults()）用 Rust default (32)

**建议**: 统一为合理值（建议 32），BusToml default 应与 BusConfig::default() 一致。

### B2 — max_concurrent_calls 语义混淆 (P0)

**文件**: `src/lib.rs:329`

**问题**: `mpsc::channel(self.config.max_concurrent_calls)` 用并发限制值作为 channel buffer size。两个概念：
- max_concurrent_calls: 同时处理的 RPC 数量上限
- channel buffer size: 等待处理的请求队列大小

如果 max_concurrent_calls=1（TOML default），只有 1 个请求能缓冲，慢 agent 导致后续请求 timeout。

**建议**: 分离为两个参数：`max_concurrent_rpc` 和 `channel_buffer_size`。

### B3 — Pipeline 循环上限硬编码 20

**文件**: `src/pipeline.rs:130`

**问题**: `for _turn in 0..20` 不可配置，20 次上限未文档化。

**建议**: `MAX_PIPELINE_ITERATIONS = 20` 常量，通过 BusConfig 或 Pipeline 构造器可配。

### B4 — InMemoryBus 480 行在 lib.rs

**文件**: `src/lib.rs` (971行)

**问题**: Bus trait + InMemoryBus 完整实现 + BusHandler + BusConfig + bus types + tests + echo_agent_loop 全在 lib.rs。

**建议**: 
- `bus_impl.rs` — InMemoryBus 实现
- `bus_types.rs` — BusError, BusRequest, BusResponse, BusEvent, AgentDescriptor 等
- `bus_handler.rs` — BusHandler, bus_handler(), bus_handler! macro
- `lib.rs` — trait 定义 + re-exports + module declarations

### B5 — run_fork_async 复制 run() 的逻辑 (160行)

**文件**: `src/pipeline.rs:460-622`

**问题**: fork closure 中的 resolve + assembly + run 逻辑与主 run() 路径 (lines 153-240) 完整复制。此外 DAG 路径不被 fork 支持。

**建议**: 提取 `execute_agent(&self, profile, resolved, input, max_turns) -> Result<Value, String>` 共享 helper。

### B6 — looks_like_error 44 行 boolean 级联

**文件**: `src/runner.rs:137-181`

**问题**: 30+ 条件包括重复（`lower.starts_with("rate limit")` overlaps `trimmed.starts_with("Rate limit")`）和不可能共现的模式。

**建议**: regex set 或结构化 matcher：
```rust
static ERROR_PATTERNS: &[&str] = &[
    "rate limit", "invalid api key", "authentication", "forbidden",
    "not found", "server error", "service unavailable", ...
];
```

### B7 — Markdown fence stripping 重复

**文件**: `src/runner.rs` + `src/micro_agent.rs`

**问题**: fence-stripping logic 在两处。

**建议**: 提取到 `utils::strip_markdown_fence()` 共享函数。

### B8 — BusHandler 三入口做相同事

**文件**: `src/lib.rs:237-265`

**问题**: `BusHandler::from_async`, `bus_handler()` free fn, `bus_handler!` macro 完全相同。

**建议**: 只保留 free fn + macro。

### B9 — SubscriberEntry.agent_id unused

**文件**: `src/lib.rs:217-221`

**问题**: `pub(crate) SubscriberEntry` 有 `agent_id: Option<AgentId>` 带 `#[allow(dead_code)]`。Set 但 never read。

**建议**: Remove 或实现 audit。

### B10 — JSON schema 验证每次 run() 重建

**文件**: `src/runner.rs:60-81`

**问题**: parsing schema string + building jsonschema validator 在每次 `AgentRunner::run()` 调用时重新计算。Invalid schema 会每次都 fail。

**建议**: 移到 profile load 阶段一次性验证 + cache。

### B11 — profile.plugins 检查每次循环执行

**文件**: `src/pipeline.rs:179-197`

**问题**: 当 `profile.plugins` 存在但 `pipeline.plugin_registry` is None，error 在每次 loop iteration 检查。

**建议**: 移到 Pipeline 构造或 dry_run 一次性验证。

### B12 — HandoffConfig.handoff_rules TOML rename 混淆

**文件**: `src/profile.rs:129-139`

**问题**: `handoff_rules` field 在 Rust 但 `rules` in TOML (`#[serde(rename = "rules")]`)。

**建议**: 字段名统一为 `rules`（已在 HandoffConfig 上下文中）。

### B13 — 4 个 TOML 中间体可消除

**文件**: `src/profile.rs:148-237`

**问题**: `BehaviorModeToml`, `PluginSlotConfigToml`, `PluginsConfigToml`, `AgentProfileRaw` 只因 PluginsConfig/PluginSlotConfig 不支持 TOML 需要的 Deserialize。

**建议**: 在 lattice-plugin 加合适 Deserialize impl，消除 ~90 行中间体。

### B14 — LatticeDir::discover_with_global 重复 discover

**文件**: `src/lattice_dir.rs:77-103`

**问题**: bus_toml 路径 resolution + bus_config loading 重复。

**建议**: 提取 private helper `load_bus_config(root: &Path)`。

### B15 — BusToml 4 个 default_* free fn

**文件**: `src/lattice_dir.rs:24-35`

**问题**: 4 个 one-liner functions for serde default。

**建议**: derive Default with explicit values 或 inline `#[serde(default = "...")]` literals。

### B16 — PERMITTED_AGENTS_DIR_PARENTS 硬编码

**文件**: `src/watcher.rs:77`

**问题**: 硬编码 Linux/macOS 路径 `/home`, `/Users`, `/tmp`, `/var/tmp`。无 Windows 支持。

**建议**: 用 `dirs` crate 跨平台。

### B17 — ws.rs WS_SECRET 读取两次

**文件**: `src/ws.rs:69+87`

**问题**: `WS_SECRET` env var 在同一 handler 中读取两次。

**建议**: 缓存第一次结果。

### B18 — DEFAULT_ALLOWED_ORIGINS 不可配

**文件**: `src/ws.rs:25-30`

**问题**: 硬编码 localhost URLs。

**建议**: 从 BusToml 或 env var 可配。

### B19 — MicroAgent subscribe handler 冗余 clone

**文件**: `src/micro_agent.rs:143-168`

**问题**: 每个 topic iteration clone `memory_for_sub` 和 `sub_agent_name`。

**建议**: clone once outside loop。

### B20 — ws.rs 应在 lattice-cli

**文件**: `src/ws.rs` (141行, feature-gated on axum)

**问题**: WebSocket HTTP serving 是 CLI/基础设施层关注点。

**建议**: 移到 lattice-cli。参见 [B8](../boundary-analysis/issues.md#b8)。

### B21 — Pipeline Cell-based memory counters not thread-safe

**文件**: `src/pipeline.rs:51-53`

**问题**: `memory_entry_count: Cell<usize>` and `memory_entry_bytes: Cell<usize>` use `Cell<T>` which is not `Sync`. If Pipeline is ever shared across threads (wrapped in `Arc`), this will fail at compile time. Currently works because Pipeline is local to a single `run()` call, but fragile.

**建议**: Replace with `AtomicUsize` (lock-free, Sync) or `Mutex<usize>` if precise ordering matters. `AtomicUsize` is the natural fit here since counters are monotonically increasing.

### B22 — Bus API signature discrepancy in wiki vs actual code

**文件**: `src/lib.rs` Bus trait definition

**问题**: Wiki documented Bus trait with old signatures (subscribe/publish with `AgentId` as required param, discover returning `AgentDescriptor`). Actual code:
- `subscribe(topic, Option<AgentId>, handler)` — agent_id is optional
- `publish(topic, event)` — source checked internally from event.source
- `discover(capability_string)` — returns Vec of descriptors matching capability, not single descriptor
- `call` and `call_with_timeout` have authorization checks internally

**建议**: Update API documentation to match actual signatures.

### B23 — Bus publish() fire-and-forget pattern

**文件**: `src/lib.rs:390-424` InMemoryBus::publish()

**问题**: Each subscriber handler is `tokio::spawn`'d — fire and forget. For `DeliveryPolicy::AtMostOnce`, errors are silently discarded. For `AtLeastOnce`, only logged as warning. No retry or dead-letter mechanism.

**建议**: Add optional retry for AtLeastOnce delivery, or at minimum accumulate failed deliveries for caller inspection.

### B24 — YoloSandboxPolicy string-to-enum mapping duplicated

**文件**: `src/profile.rs:78-88` (BehaviorConfig) + `src/profile.rs:148-190` (BehaviorModeToml)

**问题**: Two separate code paths convert TOML string "allowlist"/"nobash" to `YoloSandboxPolicy` enum:
- `BehaviorConfig.enforce_sandbox: Option<String>` — used in agent-level behavior config
- `BehaviorModeToml.to_behavior_mode()` — used in plugin slot behavior config

Both parse the same strings with slightly different logic. If new sandbox policies are added, both must be updated.

**建议**: Extract `fn parse_yolo_sandbox_policy(s: &str) -> YoloSandboxPolicy` shared helper in profile.rs.

### B25 — DefaultToolExecutor::new().expect() panics 3 times in assembly.rs

**文件**: `src/assembly.rs:68-76`

**问题**: Three `.expect("failed to build DefaultToolExecutor")` calls in assemble_agent(). If construction fails (e.g. invalid working directory), this panics instead of returning an error.

**建议**: `DefaultToolExecutor::new()` should return `Result<Self, ToolError>`. assembly.rs should propagate the error via the existing `Result<AgentRunner, Box<dyn std::error::Error>>` return type.

### B26 — Tool merging has 3 similar branches in assembly.rs

**文件**: `src/assembly.rs:53-77`

**问题**: Tool merge logic has three branches (registry present+nonempty, registry present+empty, no registry) with duplicated DefaultToolExecutor construction and tool list assembly. Each branch differs only in whether registry definitions are merged.

**建议**: Extract helper `fn build_tools_and_executor(enabled, registry) -> (Vec<ToolDefinition>, DefaultToolExecutor)` that handles all three cases.
