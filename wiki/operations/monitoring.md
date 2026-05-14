# 监控与日志

## 日志系统

lattice-core 提供 `init_logging()` 和 `init_debug_logging()`：

```rust
pub fn init_logging()     // INFO 级别，生产环境
pub fn init_debug_logging() // DEBUG 级别，开发环境
```

日志通过 `tracing` crate 实现，使用 `tracing_subscriber` 的 `fmt` subscriber。

### 关键日志点

| Crate | 模块 | 日志内容 |
|-------|------|---------|
| core | router.rs | 模型解析失败、凭证缺失、provider 选择 |
| core | invocation.rs | HTTP 请求错误、stream 错误、private IP 拦截 |
| core | streaming.rs | SSE 解析错误、chunk 解析异常 |
| agent | sandbox.rs | 命令拒绝（metachar、allowlist、hook chain） |
| agent | hook.rs | Hook 执行结果（deny/warning/lockdown） |
| agent | lib.rs | Agent run loop 事件、tool call 执行 |
| agent | memory/sqlite.rs | RwLock poison recovery（消息可能引用错误方法名，参见 A13） |
| bus | pipeline.rs | Agent 执行状态、handoff 路由、fork 并行 |
| bus | runner.rs | Schema 验证失败、looks_like_error 判断 |
| bus | registry.rs | Profile load failure（仅 warn，静默跳过，参见 A15） |
| python | engine.rs | RwLock poison recovery、credential_status |

## EventBus

Pipeline 事件通过 `EventBus` pub/sub 广播：

```rust
pub enum PipelineEvent {
    AgentStarted { name },
    AgentCompleted { name, output, duration_ms },
    Handoff { from, to },
    Fork { branches },
    PipelineCompleted { duration_ms },
    PipelineError { agent, message },
}
```

```rust
let bus = EventBus::new(256);  // 默认容量 256
bus.send(PipelineEvent::AgentStarted { name: "reviewer" });
let rx = bus.subscribe();  // broadcast::Receiver<PipelineEvent>
```

EventBus 使用 `tokio::sync::broadcast` channel，默认容量 **256**。超过容量时旧消息被丢弃。

### WebSocket 事件流

启用 `ws` feature 后，EventBus 可通过 WebSocket 推送：

```bash
# 连接 ws://localhost:PORT/ws
# 接收 PipelineEvent JSON stream
```

> `WS_SECRET` 在同一 handler 中读取两次（参见重构 #34）。`DEFAULT_ALLOWED_ORIGINS` 硬编码 localhost（参见重构 #35）。

## 健康检查

### doctor 命令

```bash
lattice doctor
```

检查项：
- Rust toolchain 版本
- 各 provider API key 状态
- 模型 catalog 加载
- SQLite FTS5 可用性
- 配置文件完整性

### stats 命令

```bash
lattice stats
lattice stats --by-model --by-provider
```

统计：
- Token 使用量（按模型/provider）
- Session 数量和大小
- Cache 命中率

### debug 命令

```bash
lattice debug <model>
```

显示：
- ResolvedModel 详情（provider、base_url、credential status）
- 不调用 LLM，仅展示解析结果

## AuditLog

Agent 的 sandbox 检查可通过 `AuditLog` 记录：

```rust
let audit = Arc::new(AuditLog::new());
let config = SandboxConfig { audit_log: Some(audit), ... };
// 拒绝事件自动记录
audit.entries()  // &[AuditEvent]
```

### AuditLog 已知问题

- `current_size: usize` 字段是死代码（`#[allow(dead_code)]`），rotation 实际用 `file.metadata()`（A11）
- `log_sync()` 丢弃 `tokio::spawn` 的 JoinHandle——异步写入错误静默丢失（A12）
- rolling file 使用 `max_entries=1000`，基于 filesystem metadata 而非 `current_size` 字段

## BusEventCollector

同步事件收集器，用于 Agent 内部收集 `ContextEvent`：

```rust
struct BusEventCollector {
    events: Vec<ContextEvent>,
}
```

简单 Vec accumulator，不涉及 bus 网络。用于 prompt compiler 的 Events layer。

## 性能指标

### TokenEstimator

```rust
// OpenAI 模型：tiktoken 精确估算
// 其他模型：char/4 估算
TokenEstimator::estimate_tokens(text, model)
```

### Credential Cache

ModelRouter 内部缓存凭证查找结果：

```rust
router.clear_credential_cache();  // 强制重新检查 env vars
```

缓存使用 `Mutex<HashMap>` 存储。每次 `resolve()` 调用检查缓存。

### Session 存储

Session 使用 SHA-256 ID + JSON 文件存储。每次对话 turn 自动保存。

### Pipeline 内存控制

```rust
DEFAULT_MAX_MEMORY_ENTRIES = 1000    // 每个 agent 内存条目上限
DEFAULT_MAX_MEMORY_BYTES = 10MB     // 内存总大小上限
DEFAULT_FORK_TIMEOUT_SECS = 120     // fork 执行超时
```

> Pipeline 使用 `Cell<usize>` 计数器，非 `Sync`——跨线程共享时不安全（参见重构 #45）。