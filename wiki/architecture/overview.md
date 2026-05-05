# 架构总览

## Crate 结构

```
lattice-cli          CLI + TUI（ratatui 终端界面）
lattice-python        PyO3 绑定（pip: lattice-core）
    ↓
lattice-bus           Pipeline / AgentRunner / TOML profile / Pub-sub event bus
    ↓
lattice-plugin        PluginDagRunner, handoff 规则语言, ErasedPlugin, PluginRegistry, 11 builtin plugins
    ↓
lattice-agent         Async Agent + tool execution（tokio）+ 7 tools + sandbox + SqliteMemory
    ↓
lattice-core          模型解析 + 推理（纯 Rust，无 PyO3）
```

依赖单向：上层依赖下层，下层不知道上层存在。无独立 `lattice-tui` 或 `lattice-harness` crate——TUI 在 `lattice-cli/src/frontend/tui/`，Pipeline/AgentRunner/profile 在 `lattice-bus`，PluginDagRunner 和 handoff 规则语言在 `lattice-plugin`。

> `lattice-memory` 和 `lattice-token-pool` 已合并到 `lattice-agent`，不再作为独立 crate。

## 核心流程

```
用户 → resolve("sonnet")
         → normalize("sonnet")
         → 查 catalog alias → "claude-sonnet-4-6"
         → 查 catalog entry → 多个 provider
         → 排序 priority，匹配 API key
         → 返回 ResolvedModel { provider, api_key, base_url, ... }

用户 → chat(resolved, messages, tools).await
         → TransportDispatcher 分发协议
         → ChatCompletionsTransport (OpenAI) 或 AnthropicTransport
         → normalize_request → POST → SSE stream
         → SseParser 解析 → StreamEvent (Token/ToolCall/Done/Reasoning)

用户 → Agent::run("prompt", 10).await
         → auto tool loop（LLM → ToolCall → execute → submit → continue）
         → 7 tools: read_file, grep（regex）, write_file, list_directory, bash, patch, web_search

用户 → Pipeline::run("start", "input").await
         → agent profile → 有 [plugins] → PluginDagRunner（DAG 编排）
                         → 无 [plugins] → AgentRunner（传统 agent 链式执行）
```

## 协议支持

| 协议 | 支持 | 说明 |
|------|------|------|
| `OpenAiChat` | 完整 | OpenAI、DeepSeek、OpenCode Go、大多数 provider |
| `AnthropicMessages` | 完整 | Anthropic、MiniMax Token Plan、OpenCode Zen |
| `GeminiGenerateContent` | 完整 | Google Gemini |

## 关键设计决策

- **模型为中心**：用户不需要知道 provider，只需知道模型名
- **凭证来自环境变量**：无配置文件，无 keychain
- **全 async**：从 core 到 CLI 全 async 链路，无 `block_on`，无全局 runtime
- **Transport trait 统一**：所有协议同一接口
- **PyO3 隔离在 python crate**：core 纯 Rust，可独立使用

## 更多

- [模型解析](model-resolution.md)
- [推理链路](inference-pipeline.md)
- [Crate 地图](crates.md)
- [安全边界](security.md)
