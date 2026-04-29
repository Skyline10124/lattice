# Artemis 当前实现状态审查

**日期**：2026-04-29  
**范围**：当前 `artemis` Rust workspace 静态扫描  
**文档性质**：当前实现快照 / 代码审查 addendum  
**重要说明**：本文件记录当前代码状态。`docs/code-review-report.md` 可视为历史审查记录；其中“生产就绪”等结论与本次扫描结果不完全一致，应以后续实际修复与验证为准。

---

## 结论摘要

当前代码已经从早期单 crate / mock engine 原型演进为分层更清楚的 Rust workspace：

- `artemis-core` 已经是纯 Rust 核心库，不再直接依赖 PyO3。
- `artemis-python` 独立承担 Python 绑定。
- `artemis-agent` 承担 conversation state、tool-call 边界和简单 retry。
- `artemis-memory` / `artemis-token-pool` 提供基础 trait 和默认实现。

整体方向是正确的，但当前实现仍然不是 README 所描述的完整 Python LLM engine。核心问题包括：

1. Python API 目前只暴露 model resolver，没有 chat / streaming / tool calling。
2. `catalog` 中大量 provider 的 `base_url: null` 未回退到 `provider_defaults`，可能导致 resolve 成功但 chat 失败。
3. `GeminiGenerateContent` transport 存在，但主 `chat()` 不支持 Gemini。
4. OpenAI SSE `[DONE]` 可能覆盖真实 `finish_reason`。
5. Anthropic streaming finish reason 与 non-streaming 映射不一致。
6. `ErrorClassifier` / `RetryPolicy` 建模存在，但没有完整贯通真实 streaming 错误链路。
7. `artemis-core/pyproject.toml` 与当前 crate 状态不匹配，疑似遗留文件。
8. Agent crate 有字段和接口雏形，但 memory、token_pool、tool result name 等尚未真正闭环。

综合判断：**当前实现适合继续开发与受控 dogfooding，不适合按 README 描述作为完整 Python 推理 SDK 对外发布。**

---

## 当前 workspace 结构

当前根目录 `artemis/` 是一个 Cargo workspace：

| Crate | 当前职责 | 当前状态 |
|---|---|---|
| `artemis-core` | model routing、HTTP/SSE、transport、streaming、errors、tokens | 核心 Rust 路径已有雏形 |
| `artemis-agent` | Agent state、tool boundary、retry | 原型可用，但未完全生产化 |
| `artemis-memory` | `Memory` trait + `InMemoryMemory` | 简单默认实现 |
| `artemis-token-pool` | `TokenPool` trait + `UnlimitedPool` | 简单默认实现 |
| `artemis-python` | PyO3 binding，包名 `artemis_core` | 当前仅 resolver |

根 `Cargo.toml` workspace members：

- `artemis-core`
- `artemis-memory`
- `artemis-token-pool`
- `artemis-agent`
- `artemis-python`

---

## 当前主调用链

### Rust core 推理链路

`artemis-core` 当前导出：

- `resolve(model)`
- `chat(resolved, messages, tools)`
- `chat_complete(resolved, messages, tools)`

实际链路：

1. `artemis_core::resolve("sonnet")`
2. `ModelRouter::resolve()`
3. 返回 `ResolvedModel`
4. `artemis_core::chat(&resolved, messages, tools)`
5. 根据 `resolved.api_protocol` 分发 transport
6. 构造 provider-native request body
7. 用共享 `reqwest::Client` 发 POST
8. 用 `reqwest-eventsource` 建 SSE stream
9. 用 provider-specific `SseParser` 转成 `StreamEvent`
10. `chat_complete()` 消费 stream 并聚合成 `ChatResponse`

当前 `chat()` 实际支持：

- `ApiProtocol::OpenAiChat`
- `ApiProtocol::AnthropicMessages`

当前 `chat()` 不支持：

- `ApiProtocol::GeminiGenerateContent`
- `ApiProtocol::CodexResponses`
- `ApiProtocol::Custom(_)`

### Python API 链路

`artemis-python/src/engine.rs` 当前只暴露：

- `ArtemisEngine.resolve_model(model)`
- `ArtemisEngine.list_models()`
- `ArtemisEngine.list_authenticated_models()`
- `PyResolvedModel` getters

当前 Python binding 没有暴露：

- `Message`
- `Role`
- `ToolDefinition`
- `ToolCall`
- `chat`
- `chat_complete`
- `run_conversation`
- streaming iterator
- `Agent`
- `submit_tools`

因此当前 Python 包实际是 **model resolver binding**，不是完整推理 engine。

---

## 已完成或较好的部分

### 1. Core 去 PyO3 化

`artemis-core` 当前 `Cargo.toml` 只构建 `rlib`，不再直接依赖 PyO3。这是正确方向：核心推理路径保持纯 Rust，Python binding 放在 `artemis-python`。

### 2. Transport trait 收敛

`artemis-core/src/transport/mod.rs` 现在定义统一 `Transport` trait，包含：

- `normalize_request`
- `denormalize_response`
- `normalize_messages`
- `normalize_tools`
- `chat_endpoint`
- `auth_header_name`
- `auth_header_value`
- `create_sse_parser`

相比早期多套 transport trait 并存，当前结构更清晰。

### 3. OpenAI / Anthropic SSE 主路径已接入

`artemis_core::chat()` 对 OpenAI Chat Completions 和 Anthropic Messages 已经执行真实 HTTP + SSE 流式处理，不再是纯 mock。

### 4. 错误类型和 Python 异常分离

`artemis-core/src/errors.rs` 定义 Rust-native `ArtemisError`，`artemis-python/src/errors.rs` 负责转 Python exceptions。职责边界比早期更清楚。

---

## 关键问题清单

### P0：对外可用性阻断

#### P0-1：Python API 与 README 不一致

README 示例中出现：

- `engine.run_conversation(...)`
- Python 传 messages 发起推理

但当前 `artemis-python` 没有这些 API。Python 用户只能 resolve/list，不能 chat。

影响：文档承诺与实际能力不一致，容易造成用户安装后无法使用。

建议：

1. 短期：修 README，明确 Python 目前只支持 resolver。
2. 中期：在 `artemis-python` 暴露 `Message`、`Role`、`ToolDefinition`、`chat_complete`。
3. 长期：暴露 streaming iterator 和 agent API。

#### P0-2：`artemis-core/pyproject.toml` 疑似遗留错误配置

当前 `artemis-core/Cargo.toml`：

- crate-type: `rlib`
- 无 `pyo3` dependency

但 `artemis-core/pyproject.toml` 仍写：

- maturin build backend
- package name `artemis-core`
- `features = ["pyo3/extension-module"]`

这与当前 crate 状态不匹配。

建议：

- 删除 `artemis-core/pyproject.toml`，或明确标记为弃用。
- 将 Python build 文档指向 `artemis-python`。
- 确认发布包只由 `artemis-python` 负责构建。

#### P0-3：catalog `base_url` 未回退到 provider defaults

`router.rs` 正常 catalog resolve 时使用：

- `pe.base_url.clone().unwrap_or_default()`

但 `catalog/data.json` 中大量 provider 的 `base_url` 是 `null`。`provider_defaults` 中虽然有默认 base URL，但正常 catalog path 没用它。

后果：

1. 用户配置某 provider 的 API key。
2. `resolve()` 可能成功返回该 provider。
3. 但 `ResolvedModel.base_url` 为空。
4. `chat()` 拼出相对 URL，如 `/chat/completions`。
5. 请求运行时失败。

建议：

- 在 router 中统一增加 `resolve_base_url(provider_entry)` 辅助函数。
- 优先级：entry base_url > provider_defaults base_url > 空字符串。
- 对需要联网的 provider，空 base_url 应视为 config error 或跳过该 provider。

---

### P1：运行时正确性问题

#### P1-1：OpenAI SSE `[DONE]` 可能覆盖真实 finish reason

`OpenAiSseParser` 遇到 finish chunk 时会发：

- `Done { finish_reason: reason }`

遇到 `[DONE]` 又会发：

- `Done { finish_reason: "stop" }`

`chat_complete()` 遇到 `Done` 不 break，会继续消费 stream 并覆盖 `finish_reason`。

后果：

- tool call 场景中真实 `finish_reason = "tool_calls"` 可能最终变成 `"stop"`。
- `artemis-agent` 中可能重复发出 `ToolCallRequired` / `Done`。

建议：

- `[DONE]` 只表示 stream transport 结束，不应生成会覆盖语义的 `Done`。
- 或者 `chat_complete()` 收到第一个 semantic `Done` 后停止处理后续 `[DONE]`。
- 添加回归测试：OpenAI tool call streaming 最终 finish_reason 必须保持 `tool_calls`。

#### P1-2：Anthropic streaming finish reason 未统一映射

`AnthropicTransport` non-streaming 会映射：

- `end_turn` → `stop`
- `tool_use` → `tool_calls`
- `max_tokens` → `length`

但 `AnthropicSseParser` streaming 当前直接返回原始 `stop_reason`。

后果：streaming 和 non-streaming 输出语义不一致。

建议：

- 在 `AnthropicSseParser` 内使用同一套 `map_stop_reason()`。
- 添加 regression test。

#### P1-3：Gemini transport 存在但主 `chat()` 不支持

`TransportDispatcher` 注册了 `GeminiTransport`，`GeminiTransport` 也有 request/response 转换测试。

但 `artemis_core::chat()` 只支持 OpenAI 和 Anthropic。Gemini resolve 可能成功，但 chat 会返回 config error。

建议二选一：

1. 短期：文档和 API 明确 Gemini 只能 resolve，不能 chat。
2. 中期：实现 Gemini non-streaming 或 streaming 主链路。

#### P1-4：ErrorClassifier / RetryPolicy 未完整贯通

`ErrorClassifier` 可以分类 HTTP 429、401、404、5xx、context overflow，但 `chat()` 的 streaming 请求没有完整使用它。

实际流式错误常见路径：

- `reqwest-eventsource` poll 阶段报错
- `EventStream` 转为 `StreamEvent::Error { message }`
- `chat_complete()` 转为 `ArtemisError::Streaming`

这样会丢失：

- status code
- provider
- retry-after
- retryable 类型

`artemis-agent` 的 retry 只包住 `chat()` 创建 stream 的阶段，未覆盖 stream consumption 阶段的 typed error。

建议：

- 在 HTTP response 建立 SSE 前检查 status/body。
- stream 错误应尽可能转 typed `ArtemisError`。
- `chat_complete()` 应在 retryable stream error 时允许 agent retry。

#### P1-5：共享 HTTP client 30 秒总 timeout 不适合 SSE

`provider.rs` 中 shared client 设置：

- connect timeout: 10s
- total timeout: 30s

对长 SSE streaming 不合适，推理模型或长回答可能被正常截断。

建议：

- 移除 global request timeout，改用 connect timeout + read idle timeout 策略。
- 或为 streaming path 使用单独 client。

---

### P2：设计和维护问题

#### P2-1：无凭证也可能 resolve 成功

如果没有 credential，`resolve()` 仍可能返回最高优先级 provider，`api_key: None`。

这对 Ollama 等 credentialless provider 合理，但对需要认证的 provider 容易误导。

建议：

- 区分 `credentialless` 和 `missing credential`。
- 对需要凭证的 provider，在没有 key 时可跳过或返回 warning-like metadata。

#### P2-2：credential cache 以 provider_id 为 key，可能污染

当前 `credential_cache` key 是 `provider_id`。

风险：

- 环境变量变化后旧 router 不刷新。
- custom model 同 provider_id 但不同 env key 时缓存可能复用错误结果。
- 注释提到 clear cache，但未看到公开方法。

建议：

- cache key 改为 `(provider_id, credential_keys fingerprint)`。
- 暴露 `clear_credential_cache()`。

#### P2-3：OpenAICompatTransport 未被主链路真正使用

`OpenAICompatTransport` 支持 custom base_url 和 extra_headers，但 `chat()` 对所有 `OpenAiChat` 使用同一个 `ChatCompletionsTransport`。

并且 `chat()` 没有应用 `transport.extra_headers()`。

影响：OpenRouter、某些 gateway、Copilot 类 provider 的特殊 header 无法表达。

建议：

- 根据 `resolved.provider` / `provider_specific` 创建具体 transport。
- 在 `chat()` request 构建阶段应用 `extra_headers()`。

#### P2-4：Agent 的 memory / token_pool 字段未接入行为

`Agent` 有：

- `memory`
- `token_pool`

但当前 `send()` / `run_chat()` 没看到实际 history save/search 或 token acquire/release。

建议：

- 要么接入行为，要么暂时标记为 reserved / TODO，避免误解。

#### P2-5：Agent tool result 缺少 tool name

`AgentState::push_tool_result()` 设置：

- `name: None`

OpenAI 可通过 `tool_call_id` 关联，但 Gemini `functionResponse.name` 需要 function name。当前 Gemini fallback 会使用 call id，当作 function name 可能不正确。

建议：

- Agent 维护 `tool_call_id -> function_name` 映射。
- `submit_tools()` 追加 tool result 时带上 name。

#### P2-6：tool result 截断可能 UTF-8 切片 panic

`push_tool_result()` 对超长结果使用 `&result[..max]`。如果 `max` 落在 UTF-8 多字节字符中间，会 panic。

建议：

- 按 char boundary 截断。
- 或按 bytes 截断后安全 fallback。

#### P2-7：CI workflow 位置可能不生效

当前 workflow 位于：

- `artemis/artemis-core/.github/workflows/ci.yml`

如果 GitHub repo root 是 `artemis/`，GitHub Actions 不会读取子目录里的 `.github/workflows`。

建议：

- 将 workflow 移到 `artemis/.github/workflows/`。
- 更新 CI 命令为 workspace root 运行。

---

## 当前 diagnostics

本次通过项目 diagnostics 看到：

1. `artemis-core/tests/e2e/regression_wave1.rs`
   - unused variable: `json`
2. `artemis-core/examples/chat_test.rs`
   - unused import: `futures::StreamExt`

未看到 diagnostics error。  
注意：本次未实际运行 `cargo test` / `cargo clippy`，因此不能据此断言完整 CI 通过。

---

## 建议修复顺序

### 第一批：让核心 runtime 不“resolve 成功但请求失败”

1. 修 `router.rs` base_url fallback。
2. 对需要凭证但无 key 的 provider 明确处理。
3. 修 `OpenAiSseParser` / `chat_complete()` finish reason 覆盖。
4. 修 Anthropic streaming finish reason 映射。
5. 将 HTTP status classification 接入 `chat()`。

### 第二批：让 Python 文档和实现一致

1. 删除或修正 `artemis-core/pyproject.toml`。
2. 更新 root README 中 Python quick start。
3. `artemis-python` 暴露基本类型：`Message`、`Role`、`ToolDefinition`。
4. 暴露 `chat_complete()`。
5. 后续再暴露 streaming iterator。

### 第三批：Agent 生产化

1. 保存 tool call id → tool name 映射。
2. 修 UTF-8 安全截断。
3. 接入 memory save/history。
4. 接入 token_pool acquire/release。
5. 设计 stream consumption 阶段 retry。

### 第四批：协议扩展

1. Gemini 主链路：实现或隐藏。
2. CodexResponses：实现或隐藏。
3. OpenAI-compatible provider-specific headers。
4. provider defaults 与 catalog entry merge 统一。

---

## 推荐验收标准

在标记“生产就绪”前，至少应满足：

- Python README 示例能在当前 binding 中实际运行。
- `resolve("sonnet")` 返回的 provider 必须有可用 `base_url`。
- OpenAI tool call streaming 最终 finish reason 保持 `tool_calls`。
- Anthropic streaming 和 non-streaming finish reason 一致。
- 429 / 401 / 404 / 5xx 在真实 HTTP path 中转成 typed `ArtemisError`。
- Agent `submit_tools()` 能保持 tool name。
- 长 streaming 不会被 30s global timeout 正常截断。
- Workspace root CI 实际运行：
  - `cargo fmt --check --all`
  - `cargo test`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cd artemis-python && maturin develop`
  - Python import smoke test

---

## 总体评价

当前项目的方向和架构拆分是对的，尤其是：

- core 纯 Rust 化
- Python binding 独立
- agent 独立
- transport 统一
- OpenAI/Anthropic SSE 主路径存在

但当前仍处于 **alpha / dogfooding 前期**：

- Rust core 有真实推理路径，但 runtime 细节仍有关键 bug。
- Python 包目前只是 resolver，不是完整 LLM SDK。
- 文档中对能力的描述超前于代码。
- 错误分类、retry、provider fallback 还没有完整贯通。

一句话总结：

> Artemis 当前已经具备“Rust model routing + OpenAI/Anthropic streaming inference core”的雏形，但还没有达到 README 所宣称的完整 Python engine / production-ready 状态。下一步应优先修 runtime correctness 和 Python API 对齐，而不是继续扩展更多 provider 名单。
