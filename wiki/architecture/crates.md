# Crate 地图

本页以当前 Cargo workspace 为准。`lattice-bus`、`lattice-python`、`lattice-registry` 是旧文档中出现过的历史名称或已移除模块，不是当前 workspace crate。

| Crate | 职责 | 主要依赖 |
|-------|------|----------|
| **lattice-core** | 模型目录、模型解析、provider transport、SSE streaming、retry、共享 chat/tool 类型 | 外部依赖 |
| **lattice-hooks** | prompt/runtime hook 基础类型与分发 | core, prompt |
| **lattice-prompt** | prompt section、layer、provider registry、system stack、tool schema provider | core |
| **lattice-session** | 会话图、commit/ref/tag/stash、turn manifest、artifact、文件和 SQLite CAS 存储 | 外部依赖 |
| **lattice-sandbox** | 读写路径、命令、URL、私网地址、Landlock 和 audit sink 策略 | 外部依赖 |
| **lattice-tools** | 内置工具 schema、能力 bundle/preset、默认工具执行器、ToolRegistry | core, sandbox |
| **lattice-agent** | Agent 状态、多轮模型循环、prompt 集成、memory、hooks、audit | core, hooks, prompt, sandbox, tools |
| **lattice-plugin** | `Plugin` trait、type-erased runner、registry、manifest loader、DAG runner、behavior policy | core, prompt, agent, sandbox, tools |
| **lattice-orchestrator** | TOML profile、AgentFactory、Pipeline、AgentRunner、EventBus、MicroAgent、watcher、可选 WebSocket | core, agent, hooks, prompt, tools, plugin |
| **lattice-runtime** | 顶层 runtime facade，集中 router、registries、security、session、pipeline、agent construction wiring | core, session, prompt, agent, plugin, sandbox, tools, orchestrator |
| **lattice-plugins** | 官方 plugin 实现集合 | core, plugin |
| **lattice-cli** | CLI、TUI、配置、凭证、session 命令和用户入口 | runtime-facing crates |

## 历史变迁

- 旧 `lattice-bus` 文档现在对应 `lattice-orchestrator`。
- `lattice-python` 当前不在 workspace 中；安装、测试和 API 文档应视为历史材料，除非重新引入对应 crate。
- `lattice-registry` 当前不存在；micro-agent registration 位于 `lattice-orchestrator`。
- `lattice-memory` 和 `lattice-token-pool` 已合并到 agent/session 相关实现，不再作为独立 crate。
- `lattice-tui` 未建成独立 crate；代码在 `lattice-cli/src/frontend/tui/`。

## 模块速览

### lattice-core

| 模块 | 作用 |
|------|------|
| `catalog/` | 模型目录、provider 默认值、协议类型 |
| `router.rs` | ModelRouter：归一化、别名、provider 选择、凭证解析 |
| `provider.rs` | ChatRequest/ChatResponse、共享 HTTP client |
| `transport/` | OpenAI-compatible、Anthropic、Gemini transport |
| `streaming.rs` | SSE parser 与 `StreamEvent` |
| `retry.rs` | retry policy 与错误分类 |
| `errors.rs` | `LatticeError` |
| `types.rs` | `Role`、`Message`、`ToolDefinition`、`ToolCall`、`FunctionCall` |

### lattice-tools / lattice-sandbox

| Crate | 模块 | 作用 |
|-------|------|------|
| tools | `tool_definitions.rs` | 内置工具 JSON schema |
| tools | `capabilities.rs` | `ToolPreset`、`ToolBundle`、能力解析 |
| tools | `executor.rs` | `DefaultToolExecutor`：文件、搜索、编辑、bash、web 工具执行 |
| tools | `tool_registry.rs` | 自定义/native 工具注册 |
| sandbox | `checks.rs` | read/write/command/URL 检查 |
| sandbox | `security.rs` | 私网/保留地址识别 |
| sandbox | `landlock.rs` | Linux Landlock 适配 |

### lattice-agent

| 模块 | 作用 |
|------|------|
| `lib.rs` | `Agent`、tool loop、`ToolExecutor`/`PluginAgent` traits |
| `state.rs` | 对话状态、消息管理、context trimming |
| `memory/` | `Memory` trait、InMemory、SQLite memory |
| `hook.rs` | hook chain，Tirith/Vet hook |
| `audit.rs` | append-only audit log |
| `events_provider.rs` | event context provider |

### lattice-plugin

| 模块 | 作用 |
|------|------|
| `lib.rs` | `Plugin` trait、behavior、runner |
| `erased.rs` / `erased_runner.rs` | type-erased plugin adapter/run loop |
| `bundle.rs` | plugin bundle metadata/defaults |
| `registry.rs` | plugin registry |
| `manifest.rs` / `loader.rs` | local plugin manifest loading |
| `dag_runner.rs` | plugin DAG 执行 |
| `orchestration.rs` | DAG edge config 和 handoff rule |

### lattice-orchestrator

| 模块 | 作用 |
|------|------|
| `profile.rs` | TOML-backed `AgentProfile` |
| `assembly.rs` | 从 profile、tool registry、system prompt、credentials 装配 Agent |
| `runner.rs` | `AgentRunner` 与 JSON schema 输出验证 |
| `pipeline.rs` | Pipeline、handoff/fork 控制流、dry-run |
| `lib.rs` | orchestrator trait、in-memory 实现、RPC/pub-sub 类型 |
| `events.rs` | `EventBus`、`PipelineEvent` |
| `micro_agent.rs` | `MicroAgent`/`MicroAgentHandle` |
| `watcher.rs` | profile hot reload |
| `ws.rs` | 可选 WebSocket 事件桥 |

### lattice-runtime / lattice-session / lattice-cli

| Crate | 作用 |
|-------|------|
| runtime | `Runtime` 和 `RuntimeBuilder`，集中模型解析、agent 构造、pipeline、security、session store |
| session | 会话持久化、CAS store、commit/ref/tag/stash/diff/GC、turn manifest |
| cli | `run`、`code`、`resolve`、`models`、`doctor`、`config`、`sessions`、`debug`、`validate`、`list`、`new`、`tui` |
