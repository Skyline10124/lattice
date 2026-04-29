# Crate 地图

| Crate | 职责 | 依赖 |
|-------|------|------|
| **artemis-core** | 模型解析、HTTP 请求、SSE 解析、retry、token 估算 | tokio, reqwest, serde |
| **artemis-agent** | Agent 状态、多轮对话、tool boundary、retry | core, memory, token-pool |
| **artemis-memory** | `Memory` trait + `InMemoryMemory` | core (Message 类型) |
| **artemis-token-pool** | `TokenPool` trait + `UnlimitedPool` | 无 |
| **artemis-python** | PyO3 绑定，pip 包 `artemis-core` | core |

## artemis-core 模块

| 模块 | 作用 |
|------|------|
| `catalog/` | data.json、模型条目、provider 默认值、ApiProtocol、ResolvedModel |
| `router.rs` | ModelRouter：归一化、别名、provider 选择、凭证解析 |
| `provider.rs` | ChatRequest/ChatResponse、共享 HTTP client |
| `transport/` | Transport trait、TransportDispatcher、ChatCompletions/Anthropic/Gemini。`NormalizedResponse` 历史上定义于此，已作为 dead code 移除 |
| `streaming.rs` | SseParser trait、OpenAiSseParser、AnthropicSseParser、StreamEvent |
| `retry.rs` | RetryPolicy（指数退避 + jitter） |
| `errors.rs` | ArtemisError 枚举、ErrorClassifier |
| `tokens.rs` | TokenEstimator（tiktoken + char/4 估算） |
| `types.rs` | Role、Message、ToolDefinition、ToolCall、FunctionCall |
