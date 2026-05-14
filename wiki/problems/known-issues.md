# 已知问题清单

> Historical note: this page is an older issue snapshot. Several entries mention removed or renamed crates such as `lattice-bus` and `lattice-python`. Use [boundary analysis](../boundary-analysis/overview.md) and the current [crate map](../architecture/crate-map.md) for active architecture work.

## 当前活跃 bug

### 1. BusConfig / BusToml 默认值不一致
- **位置**: lattice-bus/src/lib.rs + lattice_dir.rs
- **影响**: BusConfig::default().max_concurrent_calls=32 但 BusToml::default().max_concurrent_calls=1。TOML 配置和 Rust API 给不同默认值。
- **优先级**: P0
- **状态**: Open

### 2. Python stream_chat 非真增量
- **位置**: lattice-python/src/engine.rs:304-357
- **影响**: 流式响应实际是批量模式伪装成迭代器
- **优先级**: P0
- **状态**: Open

### 3. Python Message API 不完整
- **位置**: lattice-python/src/engine.rs:97
- **影响**: Python 无法发送 tool-result 或带 tool_calls 的消息
- **优先级**: P0
- **状态**: Open

### 4. validate_base_url 只在 Python 调用路径执行
- **位置**: lattice-python/src/engine.rs:215
- **影响**: Rust 直接调用 router.resolve() 的路径绕过安全检查
- **优先级**: P1
- **状态**: Open

### 5. f64::EPSILON 用于浮点比较
- **位置**: lattice-plugin/src/orchestration.rs:308-315
- **影响**: 只在值≈1.0 时有效，对 confidence 0.85 偶然可用但不正确
- **优先级**: P0
- **状态**: Open

## 代码质量问题（不影响功能但影响维护）

### 6. 11 个 builtin plugin 重复 parse_output 实现
- **位置**: lattice-plugin/src/builtin/*.rs
- **影响**: 维护时需同步 11 处
- **优先级**: P1
- **状态**: Open

### 7. looks_like_error 44 行 boolean 级联
- **位置**: lattice-bus/src/runner.rs:137-181
- **影响**: 难以理解和维护
- **优先级**: P1
- **状态**: Open

### 8. InMemoryBus 480 行在 lib.rs
- **位置**: lattice-bus/src/lib.rs
- **影响**: 代码组织混乱
- **优先级**: P2
- **状态**: Open

### 9. run_fork_async 复制主 run() 的 resolve+assembly 逻辑
- **位置**: lattice-bus/src/pipeline.rs:460-622
- **影响**: 修改 agent 构造流程时需同步两处
- **优先级**: P1
- **状态**: Open

### 10. PluginDagRunner 内联构造 Agent
- **位置**: lattice-plugin/src/dag_runner.rs:162-175
- **影响**: 重度跨 crate 耦合
- **优先级**: P1
- **状态**: Open

### 11. BehaviorMode 定义在 agent 但被 plugin/bus re-export
- **位置**: lattice-agent/src/behavior.rs → lattice-plugin/src/bundle.rs:9
- **影响**: agent 内部变化变成 plugin/bus 的 breaking change
- **优先级**: P1
- **状态**: Open

### 12. Handoff 规则引擎只在 plugin 层
- **位置**: lattice-plugin/src/orchestration.rs → lattice-bus/src/handoff_rule.rs
- **影响**: 概念性循环依赖信号
- **优先级**: P1
- **状态**: Open

### 13. JSON schema 验证每次 run() 重新构建
- **位置**: lattice-bus/src/runner.rs:60-81
- **影响**: 性能浪费
- **优先级**: P1
- **状态**: Open

### 14. Pipeline 循环上限硬编码 20
- **位置**: lattice-bus/src/pipeline.rs:130
- **影响**: 不可配置
- **优先级**: P1
- **状态**: Open

### 15. max_concurrent_calls 语义混淆
- **位置**: lattice-bus/src/lib.rs:329
- **影响**: 并发限制和 channel buffer 混为一谈
- **优先级**: P0
- **状态**: Open

## 功能缺失

### 16. Python 绑定不支持 Agent::run()
- **位置**: lattice-python 只依赖 lattice-core
- **影响**: Python 包无法使用 agent tool loop
- **优先级**: P2 (未来需求)
- **状态**: Known limitation

### 17. PluginWatcher 无 shutdown 方法
- **位置**: lattice-plugin/src/watcher.rs
- **影响**: watcher 线程永久运行
- **优先级**: P2
- **状态**: Open

### 18. AgentProfile::load() (无 hash 验证) 与 AgentRegistry (有验证) 不一致
- **位置**: lattice-bus/src/profile.rs vs registry.rs
- **影响**: 加载路径不一致
- **优先级**: P2
- **状态**: Open

### 19. SqliteMemory clone_box() panic on allocation failure
- **位置**: lattice-agent/src/memory/sqlite.rs
- **影响**: `clone_box()` uses `Box::new(*self)` which panics on OOM; `SqliteMemory` contains `SqliteConnection` (not Clone) → `.unwrap()` in `try_clone()` will panic if connection fails to reopen
- **优先级**: P1
- **状态**: Open

### 20. SqliteMemory save_entry() silently swallows all errors
- **位置**: lattice-agent/src/memory/sqlite.rs save_entry()
- **影响**: `.ok()` discards all save failures — data loss without any logging or error propagation
- **优先级**: P0
- **状态**: Open

### 21. AuditLog current_size field is dead code
- **位置**: lattice-agent/src/audit.rs `current_size: usize` field
- **影响**: Field is `#[allow(dead_code)]`, never used for rotation decisions (rotation uses filesystem metadata instead). Misleading struct definition.
- **优先级**: P2
- **状态**: Open

### 22. AuditLog log_sync() drops JoinHandle silently
- **位置**: lattice-agent/src/audit.rs `log_sync()` method
- **影响**: `tokio::spawn(write_future)` returns JoinHandle but it's immediately dropped — async write errors are silently lost
- **优先级**: P1
- **状态**: Open

### 23. Pipeline Cell-based memory counters not thread-safe
- **位置**: lattice-bus/src/pipeline.rs `memory_entry_count: Cell<usize>`, `memory_entry_bytes: Cell<usize>`
- **影响**: `Cell<T>` is not Sync — if Pipeline is shared across threads (via Arc), Cell access will panic at compile time. Currently works because Pipeline isn't shared, but fragile if architecture changes.
- **优先级**: P1
- **状态**: Open

### 24. SSE streaming hard limits not configurable
- **位置**: lattice-core/src/streaming.rs `MAX_SSE_EVENTS=10000`, `MAX_SSE_DATA_SIZE=1MB`, `MAX_SSE_BUFFER_SIZE=10MB`
- **影响**: Hardcoded limits may cause failures for very long streaming responses (e.g. code generation with 10000+ events)
- **优先级**: P2
- **状态**: Open

### 25. Copy-paste log message bugs in SqliteMemory
- **位置**: lattice-agent/src/memory/sqlite.rs
- **影响**: Poisoned lock error recovery messages reference wrong methods (e.g. say "recall_like" in save_entry and entries_by_kind methods, copy-pasted from other methods)
- **优先级**: P2
- **状态**: Open

### 26. Prompt compiler assert_eq! panic on length mismatch
- **位置**: lattice-agent/src/prompt/compiler.rs:23-27
- **影响**: `assert_eq!(sections.len(), budgets.len())` panics at runtime instead of returning Result. Callers passing mismatched sections/budgets get uncatchable crash.
- **优先级**: P1
- **状态**: Open

### 27. AgentRegistry silently skips failed profile loads
- **位置**: lattice-bus/src/registry.rs:36-42
- **影响**: Failed agent profile loads (hash verification failures, TOML parse errors) are only logged as `tracing::warn!` and silently skipped. Critical agents may disappear from pipeline without caller visibility.
- **优先级**: P1
- **状态**: Open

### 28. Memory reflect() is unimplemented stub
- **位置**: lattice-agent/src/memory/reflect.rs
- **影响**: `extract()` always returns `vec![]`. Conversation reflection feature completely unimplemented.
- **优先级**: P2
- **状态**: Open
### 29. messages_for_agent() 丢失 tool_calls 数据
- **位置**: lattice-cli/src/session.rs:215-236
- **影响**: `messages_for_agent()` 将 SessionMessage 转为 Message 时，tool_calls、reasoning_content、tool_call_id、name 都设为 None。续会话时 Agent 收不到之前的 tool call 结果，无法继续 tool loop。
- **优先级**: P0
- **状态**: Open

### 30. safe_agents_dir() 使用 process::exit(1) 而非 Result
- **位置**: lattice-cli/src/commands/mod.rs:32
- **影响**: 路径验证失败时直接退出进程，上层无法捕获做友好错误提示，也无法在 embedding 上下文中使用。
- **优先级**: P2
- **状态**: Open

### 31. TUI Bash mode 无沙箱执行
- **位置**: lattice-cli/src/frontend/tui/app.rs:747-790
- **影响**: TUI `!command` bash 模式直接调用 `tokio::process::Command::new("bash")` 执行用户输入，没有经过沙箱、命令验证或安全检查。完全绕过了 4 层防御模型（HookChain + metacharacters + env overrides + command allowlist）。
- **优先级**: P1
- **状态**: Open
