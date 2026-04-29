# 错误处理

## ArtemisError

```rust
pub enum ArtemisError {
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

## 可重试

`RateLimit` 和 `ProviderUnavailable` 可重试。默认策略：3 次，1s 基延迟，60s 最大，jitter。

## HTTP 状态码分类

| 状态码 | 错误类型 | 可重试 |
|--------|---------|--------|
| 429 | RateLimit | 是 |
| 401/403 | Authentication | 否 |
| 404 | ModelNotFound | 否 |
| 408 | ProviderUnavailable | 是 |
| 500/502/503/504 | ProviderUnavailable | 是 |
| 400 + context_length_exceeded | ContextWindowExceeded | 否 |
| 其他 | Network | 否 |
