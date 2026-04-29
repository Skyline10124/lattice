# 架构总览

## Crate 结构

```
artemis-cli          命令行（run/print/resolve/models）
artemis-tui          Ratatui TUI + Agent streaming
    ↓
artemis-harness       AgentProfile, Pipeline, AgentRunner
artemis-python        PyO3 绑定（pip: artemis-core）
    ↓
artemis-agent         AgentLoop + 对话状态 + tool boundary + 17 tools
    ↓
artemis-memory        跨 session 记忆（trait, async, SqliteMemory FTS5）
artemis-token-pool    共享 token 预算（trait）
artemis-plugin        类型化插件系统（Plugin + Behavior trait）
    ↓
artemis-core          模型解析 + 推理（纯 Rust，无 PyO3）
```

依赖单向：上层依赖下层，下层不知道上层存在。

## 核心流程

```
用户 → resolve("sonnet")
         → normalize("sonnet")
         → 查 catalog alias → "claude-sonnet-4-6"
         → 查 catalog entry → 多个 provider
         → 排序 priority，匹配 API key
         → 返回 ResolvedModel { provider, api_key, base_url, ... }

用户 → chat(resolved, messages, tools)
         → TransportDispatcher 分发协议
         → ChatCompletionsTransport (OpenAI) 或 AnthropicTransport
         → normalize_request → POST → SSE stream
         → SseParser 解析 → StreamEvent (Token/ToolCall/Done/Reasoning)
```

## 协议支持

| 协议 | chat() 支持 | 说明 |
|------|-----------|------|
| `OpenAiChat` | 完整 | OpenAI、DeepSeek、OpenCode Go、大多数 provider |
| `AnthropicMessages` | 完整 | Anthropic、MiniMax Token Plan、OpenCode Zen |
| `GeminiGenerateContent` | 仅 resolve | chat() 不支持，但 copilot/opencode 路径覆盖 Gemini 模型 |

## 关键设计决策

- **模型为中心**：用户不需要知道 provider，只需知道模型名
- **凭证来自环境变量**：无配置文件，无 keychain
- **Transport trait 统一**：所有协议同一接口
- **PyO3 隔离在 python crate**：core 纯 Rust，可独立使用

## 更多

- [模型解析](model-resolution.md)
- [推理链路](inference-pipeline.md)
- [Crate 地图](crates.md)
- [安全边界](security.md)
