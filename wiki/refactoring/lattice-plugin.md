# lattice-plugin 重构建议

## 已发现的重构机会

### P1 — 11 个 builtin plugin 重复 parse_output

**文件**: `src/builtin/*.rs` (11 files)

**问题**: 每个 plugin 的 `parse_output()` 完全相同：
```rust
fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> {
    let json = super::parse_utils::parse_json_from_response(raw)?;
    serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
}
```

**建议**: 在 Plugin trait 加 default impl：
```rust
fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError> where Self::Output: DeserializeOwned {
    let json = parse_json_from_response(raw)?;
    serde_json::from_value(json).map_err(|e| PluginError::Parse(e.to_string()))
}
```
需要 `Self::Output: DeserializeOwned` bound — 已是 trait bound。

### P2 — ErasedPluginAdapter 与 blanket impl 逻辑相同

**文件**: `src/lib.rs:237-268`

**问题**: `ErasedPluginAdapter<'a, P>` wraps `&P` as `ErasedPlugin`，但 blanket impl in `erased.rs:32-82` does the same for `T: Plugin`。两者序列化/反序列化逻辑完全相同。

**建议**: 用 macro 从同一模板生成 `T: Plugin` 和 `&P` 的 impl，消除手动重复。

### P3 — 双序列化 roundtrip

**文件**: `src/lib.rs:326-341`

**问题**: `PluginRunner::run()` 先序列化 `P::Input` → `serde_json::Value`，然后 `run_plugin_loop` 内 `to_prompt_json` 又反序列化回 `P::Input`。

**建议**: typed runner 应直接传已格式化的 prompt string 到 loop，消除 Value → Input 反序列化。

### P4 — system_prompt_delta default 重复

**文件**: `src/lib.rs:59-69` + `src/erased.rs:14-24`

**问题**: 两处 identical default impl：都 wrap `system_prompt()` in `SystemPromptDelta::agent(prompt, HashMap::new())`。

**建议**: 删除 erased.rs 的 default impl，由 blanket impl 覆盖。

### P5 — timestamp() 逻辑重复

**文件**: `src/lib.rs` + `src/dag_runner.rs:208-210`

**问题**: `timestamp()` in lib.rs 和 inline timestamp in dag_runner 都计算 Unix epoch microseconds。

**建议**: 提取到 `parse_utils::timestamp()` pub(crate) 函数。

### P6 — memory save 逻辑不一致

**文件**: `src/lib.rs:392-424` + `src/dag_runner.rs:206-221`

**问题**: PluginRunner 和 DagRunner 的 memory save 逻辑相似但 id 格式不同：
- PluginRunner: `{plugin}-user-{ts}`, `{plugin}-assistant-{ts}`
- DagRunner: `dag-{slot}-{transitions}-{ts}`

**建议**: 共享 `save_plugin_entry()` helper，统一 id scheme。

### P7 — 8 参数构造器

**文件**: `src/lib.rs` (PluginRunner::new), `src/erased_runner.rs:15-24` (run_plugin_loop + ErasedPluginRunner::new)

**问题**: `run_plugin_loop` 8 个参数，调用不可读。

**建议**: `RunLoopParams` struct + builder pattern：
```rust
struct RunLoopParams<'a> {
    plugin: &'a dyn ErasedPlugin,
    behavior: &'a dyn Behavior,
    agent: &'a mut dyn PluginAgent,
    config: PluginConfig,
    hooks: &'a dyn PluginHooks,
    retry_policy: RetryPolicy,
    memory: Option<Arc<dyn Memory>>,
}
```

### P8 — RwLock poison recovery 重复 9 次

**文件**: `src/registry.rs`

**问题**: 每个 method 都用 `self.plugins.write().unwrap_or_else(|e| e.into_inner())` 或 `.read().unwrap_or_else(...)`。

**建议**: private helpers：
```rust
fn write_plugins(&self) -> RwLockWriteGuard<HashMap<String, Arc<PluginBundle>>> { ... }
fn read_plugins(&self) -> RwLockReadGuard<HashMap<String, Arc<PluginBundle>>> { ... }
```

### P9 — O(n*m) merge

**文件**: `src/watcher.rs:78-84`

**问题**: `load_registry_bundles` 对每个 local bundle 用 `bundles.iter().position()` scan builtin list。

**建议**: HashMap 索引 O(1) lookup。

### P10 — loader.rs 冗余排序

**文件**: `src/loader.rs:36`

**问题**: `manifests.sort()` 按 path 排序后立即被 `bundles.sort_by` 按 name 重排。

**建议**: 删除第一次排序。

### P11 — manifest template rendering 不安全

**文件**: `src/manifest.rs:125-138`

**问题**: `str::replace` 无 escaping，可能 double-replacement（如果 context JSON 含 `{{input}}`）。

**建议**: 单 pass regex 或轻量 template engine。

### P12 — RefactorPlugin 依赖 CodeReviewOutput

**文件**: `src/builtin/refactor.rs:5`

**问题**: 跨 plugin 类型耦合。如果 CodeReviewOutput 变化，RefactorPlugin break。

**建议**: review field 用 `serde_json::Value` 或共享 module。

### P13 — AgentEdgeConfig 命名混淆

**文件**: `src/orchestration.rs`

**问题**: `AgentEdgeConfig` 听起来像 agent/bus 类型但属于 plugin。

**建议**: 重命名为 `DagEdgeConfig`。

### P14 — ModelRouter 每次创建

**文件**: `src/dag_runner.rs:236-241`

**问题**: 每个 slot transition 创建新 `ModelRouter::new()`，每次读 env vars。

**建议**: 构造一次或由 caller 注入。

### P15 — Cargo.toml macos_kQueue 全平台启用

**文件**: `Cargo.toml:17`

**问题**: `notify = { version = "7", features = ["macos_kQueue"] }` 全平台启用 macOS feature。

**建议**: target-specific feature cfg。

### P16 — values_equal Number/String comparison duplicated

**文件**: `src/orchestration.rs:306-318`

**问题**: `values_equal()` has two identical arms for `(Number, String)` and `(String, Number)` — both parse string as f64 and compare with f64::EPSILON. The logic is identical but written twice.

**建议**: Extract `fn float_equal(nf: f64, s: &str) -> bool` helper, or match with `(Number | String, Number | String)` pattern.

### P17 — HandoffRule.eval() any-with-empty-vec returns false silently

**文件**: `src/orchestration.rs:163-164`

**问题**: `HandoffRule.eval()` checks `all.is_empty()` explicitly (returns false), but `any` with empty Vec uses `.iter().any()` which returns false for empty iterators — same result but via different mechanism. Could be surprising if someone writes `any = []` expecting "always true" (vacuous truth).

**建议**: Add explicit `if any.is_empty() { return false; }` before the `.any()` call, matching the `all` pattern. Document that empty conditions = false.

### P18 — MAX_DAG_SLOT_TRANSITIONS and MAX_JSON_DEPTH both hardcoded 50

**文件**: `src/dag_runner.rs:19`, `src/orchestration.rs:170`

**问题**: Two independent hardcoded limits (MAX_DAG_SLOT_TRANSITIONS=50, MAX_JSON_DEPTH=50) not configurable. Different from Pipeline's MAX_PIPELINE_ITERATIONS=20.

**建议**: Make both configurable via PluginsConfig or constructor parameters. Standardize naming pattern: all limits should have `MAX_*` constants with same access pattern.