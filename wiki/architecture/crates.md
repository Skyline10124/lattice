# Crate 地图

| Crate | 职责 | 依赖 |
|-------|------|------|
| **lattice-core** | 模型解析、HTTP 请求、SSE 解析、retry、token 估算 | tokio, reqwest, serde |
| **lattice-agent** | Agent 状态、多轮对话、async tool execution（tokio）、context trimming、sandbox、7 tools | core |
| **lattice-plugin** | `Plugin` trait (Input/Output) + `ErasedPlugin` + `PluginRegistry` + 11 个内置插件 + `Behavior` (Strict/Yolo) + `PluginRunner`/`ErasedPluginRunner` | core, agent (PluginAgent) |
| **lattice-harness** | `Pipeline`（async）+ `AgentRunner` + `PluginDagRunner` + `ToolRegistry` + TOML profile + handoff + registry/watcher + SqliteMemory + WebSocket | agent, plugin, core |
| **lattice-bus** | Pub/sub event bus for inter-agent communication | 无 |
| **lattice-cli** | CLI: `run`/`resolve`/`models`/`doctor`/`bus`/`config`/`sessions`/`stats`/`debug`/`validate`/`list`/`new`/`tui`，session 自动持久化（SHA-256 ID），`--global` flags（`--json`/`--verbose`/`--continue-session`/`--session`/`--no-save`） | agent, harness, plugin |
| **lattice-tui** | Ratatui TUI + Agent streaming | agent |
| **lattice-python** | PyO3 绑定（resolver only），pip 包 `lattice-core` | core |

## 历史变迁

- `lattice-memory` 和 `lattice-token-pool` 在 Phase 3 合并回 `lattice-agent` 和 `lattice-harness`，不再作为独立 crate
- Phase 5（全 async 改造）删除了 `SHARED_RUNTIME`、`MEMORY_RT`、所有 `block_on` 桥接代码
- grep 工具从 `Command::new("grep")` 改为纯 Rust `regex` 搜索

## lattice-core 模块

| 模块 | 作用 |
|------|------|
| `catalog/` | data.json、模型条目、provider 默认值、ApiProtocol、ResolvedModel |
| `router.rs` | ModelRouter：归一化、别名、provider 选择、凭证解析 |
| `provider.rs` | ChatRequest/ChatResponse、共享 HTTP client |
| `transport/` | Transport trait、TransportDispatcher、ChatCompletions/Anthropic/Gemini |
| `streaming.rs` | SseParser trait、OpenAiSseParser、AnthropicSseParser、StreamEvent |
| `retry.rs` | RetryPolicy（指数退避 + jitter） + ErrorClassifier |
| `errors.rs` | LatticeError 枚举 |
| `tokens.rs` | TokenEstimator（tiktoken + char/4 估算） |
| `types.rs` | Role、Message、ToolDefinition、ToolCall、FunctionCall |

## lattice-agent 模块

| 模块 | 作用 |
|------|------|
| `lib.rs` | `Agent` struct（async）、`ToolExecutor` async trait、`PluginAgent` async trait |
| `state.rs` | AgentState：多轮对话状态、消息管理、context trimming |
| `tools.rs` | `DefaultToolExecutor`：7 个工具的 async 实现（tokio::fs, tokio::process, regex, reqwest） |
| `tool_definitions.rs` | 工具 JSON schema 定义 |
| `tool_error.rs` | `ToolError` 枚举（thiserror） |
| `sandbox.rs` | `SandboxConfig`：路径/命令/URL 校验 |
| `memory/` | `Memory` trait + `InMemoryMemory` + `SqliteMemory`（已迁移至 harness） |

## lattice-plugin 模块

| 模块 | 作用 |
|------|------|
| `lib.rs` | `Plugin` trait、`Behavior`/`StrictBehavior`/`YoloBehavior`、`PluginRunner`、`PluginConfig` |
| `erased.rs` | `ErasedPlugin` trait + blanket impl |
| `bundle.rs` | `PluginBundle`、`PluginMeta`、`BehaviorMode` |
| `registry.rs` | `PluginRegistry`：register/get/list |
| `erased_runner.rs` | `run_plugin_loop()`、`ErasedPluginRunner` |
| `builtin/` | 11 个内置插件：code_review, refactor, test_gen, security_audit, doc_gen, pptx_gen, deep_research, image_gen, knowledge_base, plan_gen, verification |

## lattice-harness 模块

| 模块 | 作用 |
|------|------|
| `pipeline.rs` | `Pipeline`：async 多 agent 链式/分支执行、handoff 评估 |
| `runner.rs` | `AgentRunner`：async 单 agent 执行 + JSON schema 校验 + retry |
| `dag_runner.rs` | `PluginDagRunner`：intra-agent DAG 编排 |
| `profile.rs` | `AgentProfile` + `PluginsConfig` / `PluginSlotConfig` / `AgentEdgeConfig` TOML 反序列化 |
| `tools.rs` | `ToolRegistry`、`ToolHandler`、`merge_tool_definitions()` |
| `handoff_rule.rs` | 手递规则引擎 |
| `registry.rs` | Agent 目录加载 + hot reload |
| `events.rs` | 事件总线 |
| `memory/` | SqliteMemory（FTS5）+ 共享分区访问控制 |
| `watcher.rs` | 文件变更监听 |
| `ws.rs` | WebSocket 事件推送（feature: axum） |
