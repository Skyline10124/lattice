# Crate 地图

| Crate | 职责 | 依赖 |
|-------|------|------|
| **lattice-core** | 模型解析、HTTP 请求、SSE 解析、retry、token 估算 | tokio, reqwest, serde |
| **lattice-agent** | Agent 状态、多轮对话、tool boundary、context trimming、sandbox、17 tools | core, memory, token-pool, plugin |
| **lattice-memory** | `Memory` trait (async) + `SqliteMemory` (FTS5) + `EntryKind` | core (Message 类型) |
| **lattice-token-pool** | `TokenPool` trait + `UnlimitedPool` | 无 |
| **lattice-plugin** | `Plugin` trait (Input/Output) + `Behavior` trait (Strict/Yolo) + `PluginRunner` + `PluginHooks` + `CodeReviewPlugin` | core |
| **lattice-harness** | `AgentProfile` (TOML) + `AgentRegistry` + `AgentRunner` (implicit memory recall) + `Pipeline` (sequential chaining + skip/fallback) + Python handoff | agent, memory, plugin |
| **lattice-python** | PyO3 绑定（resolver only），pip 包 `lattice-core` | core |
| **lattice-cli** | CLI: `run`/`print`/`resolve`/`models` 子命令 | agent, harness |
| **lattice-tui** | Ratatui TUI + Agent streaming | agent |

## lattice-core 模块

| 模块 | 作用 |
|------|------|
| `catalog/` | data.json、模型条目、provider 默认值、ApiProtocol、ResolvedModel |
| `router.rs` | ModelRouter：归一化、别名、provider 选择、凭证解析 |
| `provider.rs` | ChatRequest/ChatResponse、共享 HTTP client |
| `transport/` | Transport trait、TransportDispatcher、ChatCompletions/Anthropic/Gemini。`NormalizedResponse` 历史上定义于此，已作为 dead code 移除 |
| `streaming.rs` | SseParser trait、OpenAiSseParser、AnthropicSseParser、StreamEvent |
| `retry.rs` | RetryPolicy（指数退避 + jitter） |
| `errors.rs` | LatticeError 枚举、ErrorClassifier |
| `tokens.rs` | TokenEstimator（tiktoken + char/4 估算） |
| `types.rs` | Role、Message、ToolDefinition、ToolCall、FunctionCall |
