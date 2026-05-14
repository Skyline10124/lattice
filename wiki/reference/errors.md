# 错误处理

## `LatticeError` (`lattice-core`)

```rust
pub enum LatticeError {
    RateLimit { retry_after: Option<f64>, provider: String },
    Authentication { provider: String },
    ModelNotFound { model: String },
    ProviderUnavailable { provider: String, reason: String },
    ContextWindowExceeded { tokens: u32, limit: u32 },
    ToolExecution { tool: String, message: String },
    Streaming { message: String },
    Config { message: String },
    Network { message: String, status: Option<u16> },
}
```

`RateLimit` and `ProviderUnavailable` are retryable. `ErrorClassifier` maps HTTP status codes and provider response bodies into these variants.

## `ToolError` (`lattice-tools`)

```rust
pub enum ToolError {
    IoError { path: String, error: std::io::Error },
    SandboxViolation(String),
    RegexError(String),
    HttpError(String),
    CommandError(String),
    SizeLimit { limit: usize, actual: usize },
    FileNotFound(String),
}
```

The public `ToolExecutor` trait returns a result string, so tool errors are formatted through `Display` before leaving the executor.

## `SandboxError` (`lattice-sandbox`)

`SandboxError` represents policy failures from sandbox checks. Most call paths still convert sandbox failures into user-facing strings because tools report textual output to the model loop.

## `PluginError` and `DAGError` (`lattice-plugin`)

`PluginError` covers parse, validation, missing tool, context, max-turn, output-size and escalation failures in a plugin run.

`DAGError` covers plugin DAG failures such as missing entry slot, missing plugin, model resolution failure, output parse failure, unsupported fork and max slot transition exhaustion.

## `OrchestratorError` (`lattice-orchestrator`)

`OrchestratorError` covers agent registration/discovery, RPC timeouts, channel closure, serialization, model resolution, config and authorization failures.

## `RuntimeError` (`lattice-runtime`)

`RuntimeError` wraps core model errors and runtime configuration errors. Application entrypoints usually see this error type when using the `Runtime` facade.

## `SessionError` (`lattice-session`)

`SessionError` covers file/SQLite persistence, JSON serialization, invalid session IDs, missing objects and graph operation failures.

## HTTP 状态码分类

| 状态码 | 错误类型 | 可重试 |
|--------|----------|--------|
| 429 | `RateLimit` | 是 |
| 401/403 | `Authentication` | 否 |
| 404 | `ModelNotFound` | 否 |
| 408 | `ProviderUnavailable` | 是 |
| 500/502/503/504 | `ProviderUnavailable` | 是 |
| 400 + context length signal | `ContextWindowExceeded` | 否 |
| 其他 | `Network` | 否 |
