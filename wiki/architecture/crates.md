# Crate 地图

| Crate | 职责 | 依赖 |
|-------|------|------|
| **lattice-core** | 模型解析、HTTP 请求、SSE 解析、retry、token 估算 | tokio, reqwest, serde |
| **lattice-agent** | Agent 状态、多轮对话、async tool execution（tokio）、context trimming、sandbox、7 tools、SqliteMemory (FTS5)、ToolRegistry | core |
| **lattice-plugin** | `Plugin` trait (Input/Output) + `ErasedPlugin` + `PluginRegistry` + 11 个内置插件 + `Behavior` (Strict/Yolo) + `PluginDagRunner` + handoff 规则语言 | agent, core |
| **lattice-bus** | `Pipeline` + `AgentRunner` + TOML profile + JSON schema 校验 + fork 编排 + hot reload + Pub/sub event bus + microagent 通信 | core, agent, plugin |
| **lattice-cli** | CLI + TUI（ratatui）: `run`/`resolve`/`models`/`doctor`/`bus`/`config`/`sessions`/`stats`/`debug`/`validate`/`list`/`new`/`tui`，session 自动持久化（SHA-256 ID），流式聊天，slash 命令 | core, agent, bus, plugin |
| **lattice-python** | PyO3 绑定（resolver only），pip 包 `lattice-core` | core |

## 历史变迁

- `lattice-memory` 和 `lattice-token-pool` 已合并到 `lattice-agent`，不再作为独立 crate
- `lattice-harness` 不再作为独立 crate：Pipeline/AgentRunner/profile 在 `lattice-bus`，PluginDagRunner 和 handoff 规则语言在 `lattice-plugin`
- `lattice-tui` 未建成独立 crate——代码在 `lattice-cli/src/frontend/tui/`
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
| `memory/` | `Memory` trait + `InMemoryMemory` + `SqliteMemory`（FTS5） |

## lattice-plugin 模块

| 模块 | 作用 |
|------|------|
| `lib.rs` | `Plugin` trait、`Behavior`/`StrictBehavior`/`YoloBehavior`、`PluginRunner`、`PluginConfig` |
| `erased.rs` / `erased_runner.rs` | `ErasedPlugin` trait + blanket impl、`ErasedPluginRunner`、`run_plugin_loop()` |
| `bundle.rs` | `PluginBundle`、`PluginMeta`、`BehaviorMode` |
| `registry.rs` | `PluginRegistry`：register/get/list |
| `orchestration.rs` | Plugin DAG 配置类型 + canonical handoff 规则解析/求值 |
| `dag_runner.rs` | `PluginDagRunner`：intra-agent DAG 编排 |
| `builtin/` | 11 个内置插件：code_review, refactor, test_gen, security_audit, doc_gen, pptx_gen, deep_research, image_gen, knowledge_base, plan_gen, verification, superpowers |

## lattice-bus 模块

| 模块 | 作用 |
|------|------|
| `pipeline.rs` | `Pipeline`、`PipelineRun`、dry-run 校验、handoff/fork 控制流 |
| `runner.rs` | `AgentRunner`：按 `AgentProfile` 执行 `lattice_agent::Agent` |
| `profile.rs` | TOML-backed `AgentProfile`、`PluginsConfig`、handoff/memory/bus 配置 |
| `assembly.rs` | 从 profile、tool registry、system prompt、credentials 装配 `Agent` |
| `handoff_rule.rs` | 从 `lattice-plugin` 兼容导出 canonical handoff 规则语言 |
| `registry.rs` / `watcher.rs` | Agent profile 加载和 hot reload |
| `events.rs` / `micro_agent.rs` / `lib.rs` | Pipeline events、microagent RPC/pub-sub、bus 类型 |
