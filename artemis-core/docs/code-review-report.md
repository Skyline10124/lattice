# artemis-core 代码审查报告

**审查日期**: 2026-04-28
**审查范围**: `artemis-core/` 全部 30+ 源文件、测试、文档及配置
**审查维度**: 核心逻辑正确性、错误处理、性能、代码结构、安全性

---

## 问题分布概览

```
致命 (2)  ██
高   (14) ██████████████
中   (16) ████████████████
低   (12) ████████████
```

按维度分布：

| 维度 | 致命 | 高 | 中 | 低 |
|------|------|----|----|-----|
| 核心逻辑 | 1 | 4 | 1 | 6 |
| 错误处理 | 0 | 2 | 5 | 5 |
| 性能 | 0 | 3 | 3 | 2 |
| 代码结构 | 1 | 3 | 3 | 1 |
| 安全 | 0 | 3 | 2 | 1 |

---

## 一、致命问题 (Critical)

### C1. 两个相互冲突的 `Transport` trait，同名不同接口

**文件**: `src/transport/mod.rs:49` 与 `src/transport/chat_completions.rs:48`

代码中存在两个完全独立的、同名 `Transport` trait：

| trait | 位置 | 方法签名 |
|-------|------|----------|
| 格式层 `Transport` | `transport/mod.rs` | `normalize_messages()`, `normalize_tools()`, `denormalize_response()`, `denormalize_stream_chunk()` |
| HTTP 层 `Transport` | `transport/chat_completions.rs` | `base_url()`, `extra_headers()`, `api_mode()`, `normalize_request()`, `denormalize_response()` |

导致的问题：

- `AnthropicTransport` 实现格式层 trait，`GeminiTransport` 实现 HTTP 层 trait — **协议传输没有统一接口**
- 需要 `AnthropicDispatchTransport`（`transport/dispatcher.rs:25-106`）用 100+ 行适配器代码桥接两者
- `dispatcher.rs:19` 被迫使用导入别名 `use crate::transport::Transport as FormatTransport`
- 任何新增协议必须在两个不兼容的接口间选择或编写适配器

**修复建议**: 合并为单一 `Transport` trait，所有传输层实现同一接口。短期至少应重命名消除歧义。

---

### C2. AgentLoop 使用 `futures::executor::block_on`，无法运行真实 provider

**文件**: `src/agent_loop.rs:96`

```rust
let response = futures::executor::block_on(provider.chat(request));
```

`futures::executor::block_on` 只创建基础执行器，**不包含 tokio I/O reactor**。这意味着任何使用 `reqwest` 的真实 provider 调用将 panic（"there is no reactor running"）。

对比 `engine.rs:163` 正确创建了 `tokio::runtime::Runtime`：`tokio::runtime::Runtime::new().expect(...)` 并使用 `rt.block_on()`。目前 AgentLoop 仅因 `MockProvider::chat()` 不含实际 async I/O 而能工作。

**修复建议**: AgentLoop 内部持有 `tokio::runtime::Handle`，或接受 runtime 引用参数，使用 `handle.block_on(...)` 替代。

---

## 二、高优先级问题 (High)

### H1. 优先级排序跳过无需凭证的 provider

**文件**: `src/router.rs:153-166`

```rust
for pe in &sorted_providers {
    let api_key = self.resolve_credentials(pe);
    if api_key.is_some() {   // ← 跳过不需要凭证的 provider
        return Ok(ResolvedModel { ... });
    }
}
```

Ollama、Bedrock 等 provider 无需凭证（`_PROVIDER_CREDENTIALS` 中设为 `&[]`），但优先级循环将它们等同于"无凭证"跳过。结果：优先级 1 的 Ollama 反而被优先级 5 但有 API key 的 Anthropic 覆盖 — **完全违背优先级排序的语义**。

修复方向：区分"凭证未配置"和"不需要凭证"。`credential_keys` 为空且无对应环境变量的 provider 应被视为"隐式已认证"。

---

### H2. `submit_tool_result` 丢失完整对话历史

**文件**: `src/engine.rs:341-358`

```rust
let prev_messages = vec![
    Message { role: Role::Assistant, content: resp.content..., tool_calls: resp.tool_calls... },
    Message { role: Role::Tool, content: result, tool_call_id: Some(tool_call_id), ... },
];
```

每次提交工具结果时构造的 `prev_messages` 仅包含**最后一条 assistant 消息**和**当前工具结果**。所有之前的消息（原始用户输入、更早的助手回复、更早的工具结果）全部被丢弃。

`submit_tool_results`（line 244-248）循环调用 `submit_tool_result`，每次覆盖 `last_response`，第二次及后续调用只能看到碎片历史。**多轮工具对话完全断裂**。

修复方向：`EngineState` 中维护 `messages: Vec<Message>` 累积完整历史，每次 `ChatRequest` 包含全部历史。

---

### H3. AgentLoop 注入硬编码 "mock tool result"

**文件**: `src/agent_loop.rs:113-119`

```rust
conversation.push(Message {
    role: Role::Tool,
    content: "mock tool result".to_string(),
    ...
});
```

所有工具调用结果都是硬编码字符串 `"mock tool result"`。虽有 `LoopEvent::ToolCallRequired` 用于外部拦截，但缺少重入机制 — 没有 `resume_with_tool_results` 方法允许外部执行工具后将结果注入 loop。

修复方向：添加 `AgentLoop::resume_with_tool_results(results: Vec<(String, String)>)` 方法，或接受工具执行回调。

---

### H4. 两个不同的 `ErrorClassifier` 实现

**文件**: `src/errors.rs:238-293` vs `src/retry.rs:5-37`

| 差异 | `errors::ErrorClassifier` | `retry::ErrorClassifier` |
|------|--------------------------|-------------------------|
| 提取 `retry_after` | 是（解析 JSON body） | 否（始终 `None`） |
| 检测 context overflow | 是（400 + body 模式匹配） | 否 |
| 5xx 保存原因 | 是（`reason: body_lower`） | 否（`reason: "HTTP {code}"`） |
| provider 字段填充 | 是（接收 `provider: &str`） | 否（始终 `String::new()`） |
| 参数命名 | `provider: &str` | `model: &str`（概念错误） |

`agent_loop.rs:175` 导入了 `retry::ErrorClassifier` 但通过 `_classifier` 前缀**从未实际使用**。`errors::ErrorClassifier` 更完整，`retry::ErrorClassifier` 是降级副本。

修复方向：删除 `retry::ErrorClassifier`，统一使用 `errors::ErrorClassifier`。

---

### H5. Anthropic SSE 错误事件被静默吞噬

**文件**: `src/streaming.rs:318`

```rust
_ => Ok(vec![]),  // 捕获所有未识别事件类型，包括 "error"
```

当 Anthropic API 发送 SSE `error` 事件（过载、API key 无效等错误场景），解析器返回空 vec，流继续运行。调用者永远感知不到 API 错误，调试 streaming 故障几乎不可能。

修复方向：添加显式的 `"error"` 匹配分支，产生 `StreamEvent::Error { message }`。

---

### H6. Regex 每次调用重新编译

**文件**: `src/router.rs:51-60`

```rust
let mid = regex::Regex::new(r"-v\d+(:\d+)?$").unwrap()  // 每次 normalize_model_id 调用
    .replace(&mid, "").to_string();
// ...
return regex::Regex::new(r"(\d+)\.(\d+)").unwrap()       // Claude 模型额外开销
    .replace_all(&mid, "$1-$2").to_string();
```

`Regex::new()` 编译 DFA 是 Rust 生态中最昂贵的操作之一。`normalize_model_id` 位于每次模型解析的热路径上 — 在 `resolve`、`resolve_alias`、`resolve_permissive` 中被多次调用。

修复方向：使用 `LazyLock<Regex>` 或 `OnceLock<Regex>` 静态变量。

---

### H7. 每次 HTTP 请求创建新 `reqwest::Client`

**文件**: 所有 8 个 provider 文件 (~line 57-70)

```rust
let client = reqwest::Client::new();
```

每个 `chat()` 调用创建全新 HTTP Client 并在调用后丢弃。`reqwest::Client` 内部维护连接池用于 HTTP keep-alive。每次新建意味着：

- 每次 TLS 握手（包括证书链验证）
- 每次 TCP 连接建立
- 零连接复用

典型的 agent loop 每轮会多次调用 `chat()`（初轮 + 工具结果回传），此问题会累积显著延迟。

修复方向：所有 provider 共享 `LazyLock<reqwest::Client>`，配置合理 timeout。

---

### H8. `conversation.clone()` 无界增长 + `budget_tokens` 死代码

**文件**: `src/agent_loop.rs:94, 12`

```rust
let request = ChatRequest::new(conversation.clone(), tools.clone(), resolved.clone());
```

每次工具调用迭代 clone 整个累积对话（O(n) 每次迭代）。`LoopConfig` 中包含 `budget_tokens: u32` 字段（line 12），但该字段在**整个 agent loop 主体中从未被引用** — 无上下文窗口修剪、无 token 预算检查、无截断逻辑。对话会无界增长直到 `max_iterations` 达到。

修复方向：使用 `budget_tokens` 配合 `TokenEstimator::estimate_messages()` 实施 token 预算管理；或在 `ChatRequest` 中使用引用减少 clone。

---

### H9. 8 个 provider 文件几乎完全相同

**文件**: `src/providers/` 全部 8 个文件（`openai.rs`, `anthropic.rs`, `gemini.rs`, `groq.rs`, `deepseek.rs`, `mistral.rs`, `ollama.rs`, `xai.rs`）

所有 8 个 provider 的 `chat()` 实现结构几乎相同，唯一差异仅在：

1. 默认 base URL 字符串
2. HTTP 认证方式（`Bearer` vs `x-api-key` vs `x-goog-api-key`）
3. URL 路径（`/chat/completions` vs `/v1/messages` vs `models/{id}:generateContent`）

其中 5 个 OpenAI 兼容 provider（DeepSeek, Groq, Mistral, Ollama, xAI）差异仅在于默认 URL 和认证头，却各自拥有独立的 struct 类型和重复的测试套件。`tests/provider_integration.rs`（472 行）对所有 8 个 provider 重新测试相同的结构化属性。

修复方向：将共享 HTTP 逻辑提取到公共函数，OpenAI 兼容 provider 可参数化为单一 struct，通过配置区分行为。至少应将 `chat()` 的重复 HTTP 逻辑提取为共享方法。

---

### H10. `TransportDispatcher` 未被生产代码使用

**文件**: `src/transport/dispatcher.rs`

架构文档和 CLAUDE.md 都将 `TransportDispatcher` 描述为 engine 和 transport 之间的**中央路由层**。但实际代码中：

- 每个 provider 直接内部构建并使用自己的 transport
- `OpenAIProvider` 持有 `ChatCompletionsTransport` 直接使用
- `AnthropicProvider` 持有 `AnthropicTransport` 直接使用
- `GeminiProvider` 持有 `GeminiTransport` 直接使用
- Dispatcher 仅在 `transport_integration.rs` 和 `dispatcher.rs` 自身测试中被使用

修复方向：在 provider 中集成 Dispatcher，或从代码和文档中移除 Dispatcher。

---

### H11. `rig-core` 依赖从未被使用

**文件**: `Cargo.toml:20`

```toml
rig-core = "0.35"
```

在全部源代码中通过 `grep -rn 'use rig\|rig_' src/` 验证 — **零导入**。这个 0.35 主版本 crate 引入完整传递依赖树，增加编译时间且无任何收益。

修复方向：从 `Cargo.toml` 中删除此依赖。

---

### H12. API key 可能通过明文 HTTP 传输

**文件**: `src/catalog/types.rs:87-88`, `src/router.rs:626`, `src/engine.rs:419-421`

`ArtemisEngine::register_model` Python API 接受任意 `base_url` 字符串，不做 scheme 验证。注册 `http://` 的 base URL 将导致 API key 在网络上明文传输。测试中直接使用 `"http://localhost"` 作为硬编码值（`engine.rs:283`），使此问题被正常化。

修复方向：在接受 `base_url` 的入口点校验 scheme 为 `https://`（除 `localhost` 开发场景外）。对 `http://` 发出 warning。

---

### H13. 无 HTTP 请求超时

**文件**: `src/providers/openai.rs:61`, `src/providers/anthropic.rs:86`

```rust
let client = reqwest::Client::new();  // 无 timeout 配置
```

无响应服务器将导致请求永久阻塞。在 Python 绑定环境（`engine.rs:491`: `rt.block_on(entry.provider.chat(request))`）中，这会阻塞持有 GIL 的线程，**冻结整个 Python 进程**且无恢复机制。tokio runtime 本身也没有配置超时。

修复方向：在共享 `reqwest::Client` 上设置 `connect_timeout` 和 `timeout`。

---

### H14. `ResolvedModel` derive Debug 暴露明文 API key

**文件**: `src/catalog/types.rs:87`

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolvedModel {
    pub api_key: Option<String>,
    ...
}
```

任何 `dbg!()` 调用、`tracing::debug!()` 日志、或 `unwrap()` panic 消息中格式化 `ResolvedModel` 时，都会将明文 API key 输出。这是生产环境中凭据泄露的常见根源。

修复方向：为 `ResolvedModel` 手动实现 `Debug`，对 `api_key` 字段输出 `"***"`（可参考 `PyResolvedModel.__repr__` 中已有的脱敏实现，`engine.rs:40`）。

---

## 三、中优先级问题 (Medium)

### M1. Anthropic token 用量统计错误

**文件**: `src/streaming.rs:307-311`

```rust
let usage = root["usage"].as_object().map(|u| TokenUsage {
    prompt_tokens: 0,                                          // 忽略 input_tokens
    completion_tokens: u["output_tokens"].as_u64()... as u32,
    total_tokens: u["output_tokens"].as_u64()... as u32,       // 错误: 应为 input+output
});
```

Anthropic API 在 usage 块中同时返回 `input_tokens` 和 `output_tokens`。代码：
- `prompt_tokens` 始终为 0（未读取 `input_tokens`）
- `total_tokens` 等于 `output_tokens` 而非 `input_tokens + output_tokens`

修复方向：正确读取 `input_tokens` 并计算 `total_tokens = input + output`。

---

### M2. `jittered_backoff` 可能 panic

**文件**: `src/retry.rs:57-58`

```rust
let base = self.base_delay * 2u32.pow(attempt);
```

`2u32.pow(attempt)` 在 `attempt >= 32` 时通过 Rust overflow check panic。用户可构造 `RetryPolicy { max_retries: 100, .. }`，attack vector 可达。此外 jitter 可能将延迟推至 `max_delay` 以上 50%。

修复方向：使用 `saturating_pow` 或 `checked_pow`，对 attempt 做上限约束。

---

### M3. `fits_in_context` 可能 panic

**文件**: `src/tokens.rs:57` → `src/catalog/loader.rs:18`

```rust
// loader.rs
pub fn get() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let data = include_str!("data.json");
        serde_json::from_str(data).expect("Failed to deserialize catalog data.json")
    })
}
```

`fits_in_context` 是一个看似无错的 API（返回 `bool`），但在 data.json 缺失或损坏时，它通过 `Catalog::get()` 内部的 `expect()` 直接 panic 整个进程。无 Result 返回路径。

修复方向：`Catalog::get()` 应返回 `Result`，或在首次访问时温和降级。`fits_in_context` 应传播错误。

---

### M4. 所有 `From<ArtemisError> for PyErr` 在 Python 未初始化时 panic

**文件**: `src/errors.rs:146-223`（9 个转换分支）

```rust
Python::try_attach(|py| { ... }).expect("Python interpreter not initialized")
```

如果 `ArtemisError` 在 Python 上下文之外被转换（例如 Rust 原生测试、Rust 二进制调用），这 9 处 `.expect()` 会导致 panic 而非返回 fallback 错误。

修复方向：提供不使用 Python 的 fallback 路径，或确保 `ArtemisError` 仅在 Python 上下文中被转换。

---

### M5. `ToolDefinition::new` 静默吞噬无效 JSON 参数

**文件**: `src/types.rs:150-151`

```rust
let params: serde_json::Value = serde_json::from_str(parameters)
    .unwrap_or(serde_json::Value::Object(Default::default()));
```

格式错误的 JSON 参数被静默替换为空对象。用户不会收到任何错误提示，导致调试参数 schema 问题非常困难。

修复方向：在 JSON 解析失败时产生错误（`PyResult` 或 `Result`），而非静默降级。

---

### M6. `TransportDispatcher` 缺少 Bedrock 和 Codex 协议

**文件**: `src/transport/dispatcher.rs:123-139`

`ApiProtocol` 枚举有 5 个命名变体 + `Custom(String)`，但 `TransportDispatcher::new()` 只注册了 3 个：
- `OpenAiChat` — 已注册
- `AnthropicMessages` — 已注册
- `GeminiGenerateContent` — 已注册
- `BedrockConverse` — **未注册**（`dispatch()` 返回 `None`）
- `CodexResponses` — **未注册**

如果 catalog 中有模型指定了 `api_protocol: BedrockConverse` 或 `CodexResponses`，`dispatch_for_resolved` 将返回 `None`。

---

### M7. `AnthropicDispatchTransport` 丢弃 reasoning 内容

**文件**: `src/transport/dispatcher.rs:97-105`

```rust
Ok(ChatResponse {
    content: normalized.content,
    tool_calls: normalized.tool_calls,
    usage: None,                          // ← 用量数据丢失
    finish_reason: normalized.finish_reason,
    model: String::new(),                 // ← model name 为空字符串
    // reasoning: normalized.reasoning    // ← 扩展思考内容完全丢失
})
```

`NormalizedResponse` 结构体（`transport/mod.rs:33-42`）包含 `reasoning: Option<String>` 字段（用于扩展思考模型如 Claude Extended Thinking），但 `ChatResponse` 没有对应字段，dispatcher 直接丢弃。`usage` 和 `model` 也未正确传递。

---

### M8. 完整 API 错误响应体被传播到错误类型中

**文件**: `src/errors.rs:274-278, 286`, `src/provider.rs:23-24`

`ErrorClassifier::classify()` 将整个 `response_body`（lowercased）复制到 `ArtemisError::ProviderUnavailable { reason }`。fallthrough 分支将原始 body 复制到 `ArtemisError::Network { message }`。当这些错误传播到 Python（`engine.rs:464,492`），完整的响应文本（可能包含内部 IP、request ID、provider 内部细节）被暴露为 `PyRuntimeError` 字符串。

---

### M9. `base_url` 无 URL 验证

**文件**: `src/engine.rs:419-421`, `src/providers/openai.rs:48`

`register_model` Python API 接受任意 `base_url` 字符串直接用于构造请求 URL：
```rust
format!("{}/chat/completions", base_url)
```

无任何验证检查：URL 语法正确性、允许的 scheme、是否存在路径操纵。`base_url` 如 `"https://evil.com@legit.com"` 或带冲突路径的 URL 可能导致意外路由。

---

### M10. LLM 工具调用参数无大小限制

**文件**: `src/transport/anthropic.rs:50-51`

```rust
serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}))
```

解析 LLM 返回的工具调用参数时无深度或大小限制。深度嵌套或极大体积的 arguments 可能导致过度内存消耗。`unwrap_or` 静默降级也掩盖了协议错误。

---

### M11. `#![allow(deprecated)]` 在 19 个文件中重复

**文件**: 全部 19 个 `.rs` 源文件 + 2 个测试文件

代码库中仅有的真正废弃项是 `TransportType`（`types.rs:184`）。所有 19 个 `#![allow(deprecated)]` 抑制的是来自**依赖库**（主要是 PyO3 0.28）的废弃警告。后果：
- 如果 PyO3 废弃了广泛使用的 API，没有任何文件会发出警告
- 屏蔽使得无法发现真实的废弃问题
- 应在 `lib.rs` 中集中设置一次，而非重复 19 次

---

### M12. `ProviderConfig` 死代码

**文件**: `src/types.rs:222-228`

```rust
pub struct ProviderConfig {
    pub name: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub transport: TransportType,
    pub extra_headers: Option<HashMap<String, String>>,
}
```

定义、derive Serialize/Deserialize、包含 roundtrip 测试，但**从未在其自身测试之外的任何地方被使用**。架构已迁移到 `ResolvedModel`（catalog-based），`ProviderConfig` 成为遗留死代码。

---

### M13. 废弃的 `TransportType` 仍注册为 Python class

**文件**: `src/types.rs:184` 与 `src/lib.rs:65`

```rust
// types.rs
#[deprecated(since = "0.2.0", note = "Use ApiProtocol instead...")]
pub enum TransportType { ... }

// lib.rs
m.add_class::<types::TransportType>()?;
```

Python 用户可以通过 `artemis_core.TransportType.ChatCompletions` 等构造废弃类型，直接破坏到 `ApiProtocol` 的迁移路径。

---

### M14. `list_authenticated_models` 无缓存的 env var 扫描

**文件**: `src/router.rs:299-324`

```rust
for model_id in self.catalog.list_models() {
    for pe in &entry.providers {
        if self.resolve_credentials(pe).is_some() {
            authenticated.push(model_id.clone());
            break;
        }
    }
}
```

每次 `resolve_credentials` 调用都触发 `std::env::var()` 系统调用。对于 100+ 模型和每模型 3-6 个 provider，单次 `list_authenticated_models()` 调用产生 300-600 次 syscall。此方法通过 `ArtemisEngine::list_authenticated_models`（`engine.rs:473-476`）暴露给 Python，每次 Python 调用都会触发此开销。环境变量在进程生命周期内不变。

修复方向：缓存已解析的凭证（如 `HashMap<String, Option<String>>`），或在启动时一次性解析。

---

### M15. `_PROVIDER_CREDENTIALS` 线性扫描

**文件**: `src/router.rs:195-206`

```rust
for (slug, creds) in _PROVIDER_CREDENTIALS {  // 22 个条目
    if *slug == *provider_id { ... break; }
}
```

22 项 `const` 数组在每次凭证解析时执行线性扫描。应使用 `HashMap` 或 `phf` map 实现 O(1) 查找。

---

### M16. HTTP 响应体双重分配

**文件**: 所有 provider（如 `src/providers/openai.rs:67-71`）

```rust
let text = resp.text().await.map_err(...)?;       // String 分配
let json: serde_json::Value = serde_json::from_str(&text).map_err(...)?;  // Value 分配
```

`resp.text()` 将整个响应体读入 `String`。然后 `serde_json::from_str` 从该 String 创建 `Value`。String 分配是对 JSON 数据的完整复制，解析后立即被丢弃。对于大响应（如含工具调用结果），内存使用加倍。

修复方向：使用 `resp.json::<serde_json::Value>().await` 直接从网络缓冲区流式解析到 JSON，避免中间 String。

---

## 四、低优先级问题 (Low)

| # | 问题 | 文件:行 |
|---|------|---------|
| L1 | OpenAI SSE: 参数 delta 在无前置 ToolCallStart 时被静默丢弃（无法诊断部分工具调用丢失） | `streaming.rs:174-179` |
| L2 | `extract_model_from_body` 对含空格 model ID 会截断 | `errors.rs:332-334` |
| L3 | `extract_retry_after` 无法解析字符串编码的数值（如 `"retry_after": "30"`，这是合法 JSON） | `errors.rs:309-315` |
| L4 | HTTP 408（Request Timeout）被分类为普通 `Network` 错误，而非可重试 — 语义上 408 更接近 503/429 | `errors.rs:249-292` |
| L5 | `EventStream::poll_next` 将传输级错误映射为非终止 `StreamEvent::Error`，调用者无法区分致命传输错误和可恢复 API 错误 | `streaming.rs:464-468` |
| L6 | SSE `buffer.extend(events)` 无界增长，恶意或 buggy 上游可导致内存耗尽 | `streaming.rs:382-384` |
| L7 | `resolve_permissive` 接收未归一化的 `model_name`（非 `normalized`），大写 provider name 匹配失败 | `router.rs:110, 246` |
| L8 | `ArtemisEngine::run_once` 硬编码 provider 为 `"mock"`、api_key 为 `None`、base_url 为 `"http://localhost"` — 与 `resolve_model` 返回的路由信息不一致 | `engine.rs:277-288` |
| L9 | `ChatRequest.model` 与 `resolved.api_model_id` 语义冗余，可直接构造方式使两者分歧 | `provider.rs:42-48, 58` |
| L10 | 两个独立的 `MockProvider` 实现（`mock.rs:28` 作为公共模块，`provider.rs:208` 在 `#[cfg(test)]` 中），无法共享测试基础设施 | 两处 |
| L11 | 架构文档描述 dispatcher → transport 流程，与实际 provider 直接调用 transport 的代码路径不符 | `docs/architecture.md` |
| L12 | `submit_tool_result` 从 Python 接收 `tool_call_id: String` 无大小或字符集验证，10MB 字符串会被直接序列化到 API 请求体 | `engine.rs:330-333` |

---

## 五、修复路线图

### 第一阶段：立即可修复（低成本，高影响）

| 优先级 | 问题 | 行动 |
|--------|------|------|
| 1 | H11 | 从 Cargo.toml 删除 `rig-core` 依赖 |
| 2 | H4 | 删除 `retry::ErrorClassifier`，统一使用 `errors::ErrorClassifier` |
| 3 | M11 | 删除各文件中的 `#![allow(deprecated)]`，仅在 `lib.rs` 保留（或缩小到特定 PyO3 lint） |
| 4 | M12 | 删除 `ProviderConfig` 类型及测试 |
| 5 | M13 | 从 `lib.rs` 中移除 `TransportType` 的 `add_class` 注册 |
| 6 | H6 | Regex 改为 `LazyLock<Regex>` 静态变量 |
| 7 | H14 | 为 `ResolvedModel` 手动实现 `Debug`，脱敏 `api_key` |
| 8 | M2 | `jittered_backoff` 使用 `saturating_pow` 防止溢出 |

### 第二阶段：核心逻辑修复（中等成本）

| 优先级 | 问题 | 行动 |
|--------|------|------|
| 9 | H2 | 修复 `submit_tool_result` 对话历史丢失 — `EngineState` 中维护 `messages` 向量 |
| 10 | H1 | 修复优先级排序：credentialless provider 应被视为"已认证" |
| 11 | H5 | Anthropic SSE `error` 事件添加显式匹配分支 |
| 12 | M1 | 修复 Anthropic usage 统计（读取 `input_tokens`，正确计算 total） |
| 13 | H12 | `base_url` 添加 HTTPS 校验（localhost 例外） |
| 14 | H13 | 共享 `reqwest::Client` 配置 connect_timeout 和 timeout |
| 15 | H7 | 所有 provider 共享 `LazyLock<reqwest::Client>` |

### 第三阶段：架构改进（较高成本）

| 优先级 | 问题 | 行动 |
|--------|------|------|
| 16 | C1 | 合并两个 `Transport` trait 为统一接口 |
| 17 | H9 | 提取公共 provider `chat()` 逻辑，参数化 OpenAI 兼容 provider |
| 18 | C2 | AgentLoop 集成 tokio runtime |
| 19 | H3 | AgentLoop 添加 `resume_with_tool_results` 重入机制 |
| 20 | H8 | 实现 `budget_tokens` 上下文窗口管理 |
| 21 | H10 | 在 provider 中集成 TransportDispatcher，或移除 Dispatcher 并更新架构文档 |
| 22 | M14 | 添加凭证解析缓存 |
| 23 | M15 | `_PROVIDER_CREDENTIALS` 改为 HashMap |
| 24 | M16 | 使用 `resp.json::<Value>()` 代替 `resp.text()` + `from_str` |
| 25 | M6 | 为 Bedrock 和 Codex 协议添加 transport 注册 |

### 第四阶段：边界情况打磨

| 优先级 | 问题 | 行动 |
|--------|------|------|
| 26 | M3 | `Catalog::get()` 和 `fits_in_context` 改为返回 Result |
| 27 | M4 | `From<ArtemisError> for PyErr` 添加非 Python fallback 路径 |
| 28 | M5 | `ToolDefinition::new` 在无效 JSON 时返回错误 |
| 29 | M8 | 限制传播到错误消息中的响应体大小 |
| 30 | M9 | `base_url` 添加 URL 格式验证 |
| 31 | L1-L12 | 各低优先级项逐步处理 |

---

## 六、正面发现

审查中也确认了以下良好实践：

- **无 `unsafe` 代码**：所有 Python FFI 通过 PyO3 安全抽象完成，无手动 unsafe 块
- **GIL 处理正确**：`Python<'_>` token 在阻塞操作前正确获取（`engine.rs:481`）
- **API key 脱敏**：`PyResolvedModel.__repr__` 正确将 API key 显示为 `***`（`engine.rs:40`）
- **测试覆盖充分**：核心类型均有 roundtrip 测试（serde 序列化/反序列化），SSE 解析器有详尽单元测试覆盖文本流、工具调用流、错误流
- **架构文档完备**：`docs/architecture.md` 精确描述了模块边界、数据流和设计意图
- **错误分类清晰**：`errors::ErrorClassifier` 的 HTTP 状态码映射逻辑完整且有序
- **SSE 解析器设计良好**：`SseParser` trait 实现可插拔解析策略，测试中 `parse_sse_text` 函数支持无网络连接测试
- **模块化程度高**：catalog、router、provider、transport、streaming 各层有清晰的职责边界

---

*本报告由 Claude Code 自动代码审查生成。审查基于对全部源文件的静态分析，未运行动态测试或构建验证。*
