# 性能优化

## Token 估算

`lattice-core::tokens::TokenEstimator` 提供 token 估算：

- **OpenAI 模型**: 使用 tiktoken 精确计算（需要 `tiktoken-rs` feature）
- **其他模型**: char/4 规则估算（简单但偏差较大）

### Context trimming

Agent 自动在每次对话前调用 `trim_messages()`：

```rust
state.trim_messages(context_length, safety_margin_percent);
```

- 按 safety_margin_percent（默认 10%）留出余量
- 保留所有 System 消息
- 从最旧的 User/Assistant 消息开始删除
- 至少保留 1 个 User + 1 个 Assistant 对

## Credential Cache

`ModelRouter` 内部缓存凭证查找：

- 缓存 key: `(provider_id, credential_env_var_name)`
- 存储: `Mutex<HashMap<(String, String), Option<String>>>`
- 生命周期: 直到 `clear_credential_cache()` 调用或进程退出

缓存避免每次 `resolve()` 都读取环境变量。

## HTTP 客户端

`lattice-core::provider` 使用共享 `reqwest::Client`：

```rust
static SHARED_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())  // 禁止 HTTP 重定向（SSRF 防护）
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120))       // chunk 间隔超时，不影响总流时长
        .build()
        .expect("Failed to build shared reqwest::Client")
});
```

- 连接池复用 TCP 连接
- **禁止 HTTP 重定向**（`redirect(Policy::none())`）— 防止 redirect-based SSRF
- connect_timeout: 10s
- read_timeout: 120s — 仅影响 chunk 间隔，SSE stream 中每个 chunk 重置计时器
- 无总时长限制 — 长 streaming 响应不会被杀

## SSE 解析

`sse_from_bytes_stream` 直接处理 HTTP bytes_stream：

- 无 `reqwest-eventsource` 依赖
- 手动行解析：`parse_raw_sse()` → `RawSseEvent` → `SseParser::parse_chunk()` → `StreamEvent`
- 三种 parser（OpenAI, Anthropic, Gemini）都是 **stateful**——跨 chunk 跟踪 tool call ID

### Python 流式优化

当前 `stream_chat` 实现 collect 整个 Stream 到 Vec，丧失增量延迟。

优化方案：
```rust
// 使用 tokio::sync::mpsc channel
let (tx, rx) = tokio::sync::mpsc::channel(32);

// async task 推送事件
tokio::spawn(async move {
    while let Some(event) = stream.next().await {
        tx.send(event).await.ok();
    }
});

// StreamIterator.__next__() 从 channel 取单个事件
```

参见 [重构 #5](../refactoring/overview.md)（P0 级）。

## Retry 策略

默认 `RetryPolicy`:
- max_retries: 3
- base_delay_ms: 1000 (1s)
- max_delay_ms: 60000 (60s)
- jitter: true (随机抖动避免 thundering herd)

重试仅对 `RateLimit` 和 `ProviderUnavailable` 错误生效。

## Benchmark

```bash
# 运行 benchmark
cargo bench -p lattice-core

# 主要 benchmark 指标
# - token 估算速度
# - SSE 解析速度
# - resolve() 延迟
```

## Memory 选择

| Memory 类型 | 适用场景 | 性能 |
|-------------|---------|------|
| InMemoryMemory | 测试、短期对话 | O(1) store, O(n) search |
| SqliteMemory (FTS5) | 生产、长期记忆 | FTS5 全文搜索，持久化到磁盘 |

SqliteMemory 使用 `rusqlite`（bundled SQLite + FTS5）。数据存储在 `.lattice/memory.db`。

## 并行 Fork

Pipeline 的 fork 模式使用 `tokio::spawn` 并行执行多个 agent：

```rust
// fork:A,B → 同时运行 agent A 和 B
// 结果合并为 {A: output, B: output} JSON
```

fork 结果通过 `merge_fork_outputs()` 合并。每个 fork branch 独立 `ModelRouter` + `Agent`。

## Profile 加载优化

- blake3 hash 验证：`load_profile_verified()` 确保 profile 文件未被篡改
- Hot reload：`Watcher` 监控 agent 目录变更，自动重新加载
- Schema 验证：每次 `AgentRunner::run()` 重新构建 jsonschema validator（**应优化为一次性**）

参见 [重构 #18](../refactoring/overview.md)。