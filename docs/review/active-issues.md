# LATTICE 活跃问题追踪

**最后更新**: 2026-05-01
**CI 基线**: 532 passed / 4 ignored / 0 failed, clippy clean, fmt clean
**审核来源**: lattice-core 全量代码审核（17 文件, 11245 行）

---

## 严重度定义

| 等级 | 含义 | 响应要求 |
|------|------|---------|
| P0 | 对外可用性阻断 | 立即修复 |
| P1 | 运行时正确性/安全缺陷 | 本迭代修复 |
| P2 | 设计/安全/维护问题 | 计划中 |
| P3 | 代码质量改进 | 延后 |

---

## 统计

| 等级 | 数量 |
|------|------|
| P1 | 9 |
| P2 | 18 |
| P3 | 13 |
| **总计** | **40** |

---

## P1 — 安全/逻辑 HIGH（9 项）

### CORE-H1：ResolvedModel.api_key Serialize 泄露密钥

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H1 |
| **等级** | P1 |
| **维度** | 安全 |
| **组件** | lattice-core/catalog/types.rs |
| **位置** | `types.rs:91-92` |
| **状态** | OPEN |

**描述**：`ResolvedModel::api_key: Option<String>` derive `Serialize`。`Debug` impl 正确遮掩为 `"***"`，但 `serde_json::to_string(&resolved_model)` 输出明文密钥。任何日志、缓存、IPC 系统序列化此结构都会泄露密钥。

**建议修复**：`api_key` 字段加 `#[serde(skip)]` 或改用 `Zeroize<String>`；需要序列化时用专用不含密钥的结构体。

---

### CORE-H2：provider_specific header: 注入无 allowlist

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H2 |
| **等级** | P1 |
| **维度** | 安全 |
| **组件** | lattice-core |
| **位置** | `lib.rs:76-79`, `gemini.rs:418-421`, `transport/mod.rs:243-249` |
| **状态** | OPEN |

**描述**：`provider_specific` 中任何以 `"header:"` 开头的键直接注入 HTTP header，无名称/值验证。可覆盖 `Authorization`、`Host` 等敏感 header。catalog 是编译时嵌入（风险可控），但 `register_model()` API 允许库消费者注入任意 header。

**建议修复**：添加 header name allowlist（只允许非敏感 header），或拒绝 `authorization`/`host`/`cookie` 等受保护名称。

---

### CORE-H3：ApiProtocol serde 与 FromStr 不一致

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H3 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/catalog/types.rs |
| **位置** | `types.rs:17-29 vs 34-41` |
| **状态** | OPEN |

**描述**：`FromStr` 接受 `"anthropic"` 作为 `AnthropicMessages` 的简写，但 serde deserialize 只认 `"anthropic_messages"`。TOML/JSON 用户写 `"anthropic"` 静默变成 `Custom("anthropic")`，运行时报 "Streaming not yet supported for protocol Custom(...)"。

**建议修复**：serde 改用 `try_from` + 自定义 deserialize，统一接受简写；或文档明确只接受全称。

---

### CORE-H4：serde(untagged) Custom 吞拼写错误

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H4 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/catalog/types.rs |
| **位置** | `types.rs:28` |
| **状态** | OPEN |

**描述**：`#[serde(untagged)]` `Custom(String)` 捕获所有未知字符串，包括拼写错误如 `"chat_compltions"`。不产生 deserialize 错误，运行时报误导性 "not supported for Custom"。

**建议修复**：移除 `untagged`，对未知字符串返回 deserialize 错误，或至少 log warning。

---

### CORE-H5：estimate_messages 遗漏 tool_calls/reasoning_content/name

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H5 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/tokens.rs |
| **位置** | `tokens.rs:49-54` |
| **状态** | OPEN |

**描述**：`estimate_messages_for_model` 只计算 `m.content`，忽略 `m.tool_calls`、`m.reasoning_content`、`m.name`。Tool-heavy 对话估算严重偏低，`fits_in_context` 可能误报"可以放进去"导致 API 拒绝。

**建议修复**：估算中加上 tool_calls JSON arguments 长度、name 字段、reasoning_content 长度。

---

### CORE-H6：畸形 API 响应返回 Ok 而非 Err

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H6 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/transport/chat_completions.rs |
| **位置** | `chat_completions.rs:190-253` |
| **状态** | OPEN |

**描述**：当 `response["choices"]` 缺失或非 array，`denormalize_response` 返回 `Ok(ChatResponse { content: None, finish_reason: "stop", model: "unknown" })`。HTML 错误页解析为 JSON 时产生合法 `None`，与真正空响应不可区分。

**建议修复**：`choices` 缺失或 `content` + `tool_calls` 都为 None 时返回 `Err(LatticeError)`。

---

### CORE-H7：GeminiTransport trait 默认值全是 OpenAI 的

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H7 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/transport/gemini.rs |
| **位置** | GeminiTransport impl Transport |
| **状态** | OPEN |

**描述**：GeminiTransport 不 override `chat_endpoint()`（默认 `/chat/completions`）、`create_sse_parser()`（默认 OpenAiSseParser）、`auth_header_name/value()`（默认 `Authorization: Bearer`）。所有 trait 默认值对 Gemini 错误。Gemini 通过独立的 `send_gemini_nonstreaming_request` 绕过 trait。

**建议修复**：override 所有 trait 方法返回 Gemini 正确值，或 Gemini 不实现 Transport trait 而用独立入口。

---

### CORE-H8：Gemini streaming finish_reason 与 non-streaming 不一致

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H8 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/transport/gemini.rs |
| **位置** | `gemini.rs:359-362` vs `parse_response` |
| **状态** | OPEN |

**描述**：`parse_response` 在有 tool calls 时将 finish_reason override 为 `"tool_calls"`，但 `denormalize_stream_chunk` 映射原始 `"STOP"` 为 `"stop"`，不检测 tool calls 存在。同一响应两条路径产生不同 finish_reason，streaming 模式下游错过工具执行。

**建议修复**：`denormalize_stream_chunk` 检测 function calls 存在时 override finish_reason 为 `"tool_calls"`。

---

### CORE-H9：Anthropic anthropic-version header 不在 transport

| 字段 | 值 |
|------|-----|
| **ID** | CORE-H9 |
| **等级** | P1 |
| **维度** | 逻辑 |
| **组件** | lattice-core/transport/anthropic.rs |
| **位置** | 缺失（只在 lib.rs 调用点注入） |
| **状态** | OPEN |

**描述**：`anthropic-version: 2023-06-01` 是 Anthropic API 必需 header，但不在 `AnthropicTransport.extra_headers()` 中设置，只在 `lib.rs:184` 通过 `extra_headers` 参数传递。Transport 单独使用时（绕过 `chat()`）请求缺此 header，API 返回错误。

**建议修复**：`AnthropicTransport::new()` 将 `anthropic-version` 加入 `extra_headers`。

---

## P2 — 安全/逻辑 MEDIUM（18 项）

### CORE-M1：validate_base_url 定义但未在 resolve 流程中调用

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M1 |
| **维度** | 安全 |
| **位置** | `router.rs:569-601` |
| **状态** | OPEN |

`validate_base_url()` 检查 HTTPS，但 `resolve()`、`resolve_permissive()`、`register_model()` 均不调用。`http://evil.server.com` 可通过。

---

### CORE-M2：Debug 日志文件默认权限 0o644

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M2 |
| **维度** | 安全 |
| **位置** | `logging.rs:68-71` |
| **状态** | OPEN |

`init_debug_logging` 创建日志文件无权限控制，world-readable。trace 日志含请求/响应体和工具调用参数中的敏感数据。

---

### CORE-M3：init_debug_logging log_path 无路径校验

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M3 |
| **维度** | 安全 |
| **位置** | `logging.rs:61` |
| **状态** | OPEN |

`log_path` 直接传给 `fs::create_dir_all` 和 `fs::OpenOptions::open`，`"../../etc/cron.d/malicious"` 可写入。

---

### CORE-M4：SSE 解析无事件大小/数量上限

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M4 |
| **维度** | 安全 |
| **位置** | `streaming.rs:381-423` |
| **状态** | OPEN |

恶意 SSE server 可发送超长 `data:` 行或百万级小事件，无限制导致 OOM。与 SSE buffer O(n^2) 重分配结合更严重。

---

### CORE-M5：Gemini api_model_id URL 拼接无编码

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M5 |
| **维度** | 安全 |
| **位置** | `gemini.rs:410` |
| **状态** | OPEN |

`format!("{}/models/{}:generateContent", base_url, model)` 中 `model` 未 URL-encode。含 `/`、`?`、`#` 的 model ID 可产生路径注入或 SSRF。

---

### CORE-M6：auth_header_name/value 独立 override 的 footgun

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M6 |
| **维度** | 安全 |
| **位置** | `transport/mod.rs:243-249` |
| **状态** | OPEN |

默认 `auth_header_value` 格式化为 `Bearer {key}`。只 override `auth_header_name` 不 override `auth_header_value` 会发 `x-api-key: Bearer sk-xxx`（Anthropic 期望裸 key）。

---

### CORE-M7：Gemini chat() 非.streaming

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M7 |
| **维度** | 逻辑 |
| **位置** | `lib.rs:189-207` |
| **状态** | OPEN |

Gemini 调 `send_gemini_nonstreaming_request` 全量收集后才返回，chat() 声称 streaming 但 Gemini 实际不 streaming。

---

### CORE-M8：resolve_permissive 硬编码 context_length 131072

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M8 |
| **维度** | 逻辑 |
| **位置** | `router.rs:473` |
| **状态** | OPEN |

未知 model 通过 permissive 路径 resolve 时 context_length 固定 128K，无法覆盖，误导 token budget 逻辑。

---

### CORE-M9：normalize_model_id 嵌套 provider 前缀残留

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M9 |
| **维度** | 逻辑 |
| **位置** | `router.rs:59-63` |
| **状态** | OPEN |

`split_once('/')` 只 strip 第一段 `"openrouter"`，`"openrouter/anthropic/claude-sonnet-4.6"` 残留 `"anthropic/"` 导致 catalog lookup 失败。

---

### CORE-M10：ProviderUnavailable.reason casing 不一致

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M10 |
| **维度** | 逻辑 |
| **位置** | `errors.rs:158 vs 170` |
| **状态** | OPEN |

5xx 路径存 lowercased body，400/overloaded 路径存原始 case。

---

### CORE-M11：retry_after 可解析为负数

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M11 |
| **维度** | 逻辑 |
| **位置** | `errors.rs:277-279` |
| **状态** | OPEN |

`take_while` 包含 `-` 字符，`"retry_after": -5` 解析为 `Some(-5.0)`。

---

### CORE-M12：fits_in_context 用 < 而非 <= 或留 margin

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M12 |
| **维度** | 逻辑 |
| **位置** | `tokens.rs:61` |
| **状态** | OPEN |

`estimated == context_length` 时返回 fits，但 provider 在极限实际拒绝。应留 margin 或用 `<=` 加负偏移。

---

### CORE-M13：缺失 tool arguments 默认 "{}"

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M13 |
| **维度** | 逻辑 |
| **位置** | `chat_completions.rs:215-218` |
| **状态** | OPEN |

API 响应中 tool call 缺 `arguments` 字段时默认 `"{}"`，掩盖协议违规。

---

### CORE-M14：Anthropic tool_use 缺 id/name 默认空字符串

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M14 |
| **维度** | 逻辑 |
| **位置** | `anthropic.rs:99-108` |
| **状态** | OPEN |

`tool_use` block 缺 `id`/`name` 时默认空字符串而非报错，导致 tool result round-tripping 失败。

---

### CORE-M15：chat_response_to_stream 不发 ToolCallEnd

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M15 |
| **维度** | 逻辑 |
| **位置** | `transport/mod.rs:63-74` |
| **状态** | OPEN |

`chat_response_to_stream` 发 ToolCallStart + ToolCallDelta 但不发 ToolCallEnd，工具调用生命周期不闭合。

---

### CORE-M16：Gemini 空 User/Assistant 消息静默丢弃

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M16 |
| **维度** | 逻辑 |
| **位置** | `gemini.rs:111-119, 120-145` |
| **状态** | OPEN |

空 content 的 User/Assistant 消息被整体跳过，Anthropic 插入占位文本。对话历史不一致可能导致 Gemini API 拒绝。

---

### CORE-M17：Gemini temperature 不防 NaN/Infinity

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M17 |
| **维度** | 逻辑 |
| **位置** | `gemini.rs:497-498` |
| **状态** | OPEN |

直接 `json!(temp)`，其他 transport 用 `apply_temperature()` 有 NaN/Infinity guard。

---

### CORE-M18：CLAUDE.md 说 env-only credentials 但 with_credentials 存在

| 字段 | 值 |
|------|-----|
| **ID** | CORE-M18 |
| **维度** | 文档 |
| **位置** | `CLAUDE.md:127` vs `router.rs:113-117` |
| **状态** | OPEN |

文档说 "Credentials come from **environment variables only**"，但 `ModelRouter::with_credentials(creds)` 接受外部注入优先于 env vars。

---

## P3 — 逻辑/文档/质量 LOW（13 项）

### CORE-L1：stream: false 不写入 body

| 位置 | `transport/mod.rs:287-291` |
| **状态** | OPEN |

`set_stream_flag` 只在 `stream=true` 时写入 `"stream": true`，`stream=false` 时缺字段。Ollama 等默认 streaming 的 provider 行为不一致。

---

### CORE-L2：temperature from_f64 失败 fallback 到 0

| 位置 | `transport/mod.rs:276-278` |
| **状态** | OPEN |

`serde_json::Number::from_f64(temp)` 返回 None 时 fallback 为 `Number::from(0)`（贪婪解码），而非 omit 或 warn。与 NaN/Infinity 分支行为不一致。

---

### CORE-L3：contains("gpt-4o") 被 starts_with("gpt-") 覆盖是死代码

| 位置 | `tokens.rs:9,13` |
| **状态** | OPEN |

---

### CORE-L4：is_openai_model 对 "o3"/"o4" 前缀有 false positive

| 位置 | `tokens.rs:7-14` |
| **状态** | OPEN |

任何以 "o3" 开头的字符串都被当作 OpenAI 模型，包括 `"o3000-custom"` 等非 OpenAI 模型名。

---

### CORE-L5：retry jitter 在 base >= max_delay 时无效

| 位置 | `retry.rs:24-28` |
| **状态** | OPEN |

当 `base >= max_delay` 时 jitter 加了 50% 但 `min` 重新 clamp 到 `max_delay`，碰撞避让完全失效。

---

### CORE-L6：from_data 静默丢弃重复 canonical_id

| 位置 | `catalog/loader.rs:35-39` |
| **状态** | OPEN |

data.json 中相同 canonical_id 的第二条记录静默覆盖第一条，无 warning。

---

### CORE-L7：Gemini tool result 启发式 JSON 解析

| 位置 | `gemini.rs:151-158` |
| **状态** | OPEN |

以 `{` 或 `[` 开头的 content 强制解析为 JSON，`unwrap_or_else` 静默捕获解析失败。

---

### CORE-L8：Gemini "OTHER" finish reason 映射为 "stop"

| 位置 | `gemini.rs:91` |
| **状态** | OPEN |

`"OTHER"` 表示"未知原因结束"，映射为 `"stop"`（正常完成）语义不同。

---

### CORE-L9：Gemini streaming vs non-streaming tool call ID 不稳定

| 位置 | `gemini.rs:95,349` |
| **状态** | OPEN |

`parse_response` 和 `denormalize_stream_chunk` 对同一 tool call 产生不同 ID。

---

### CORE-L10：SSE buffer 每次事件重分配 O(n^2)

| 位置 | `streaming.rs:454` |
| **状态** | OPEN |

`buf = buf[pos + 2..].to_string()` 每次创建新 String，高吞吐 streaming 有性能风险。

---

### CORE-L11：OpenAiSseParser ToolCallEnd 顺序不确定

| 位置 | `streaming.rs:175-176` |
| **状态** | OPEN |

`HashMap::drain()` 迭代顺序不确定，多个 tool call 的 End 事件顺序随机。

---

### CORE-L12：多处文档与代码不符

| 位置 | 多处 |
| **状态** | OPEN |

- `CLAUDE.md` 模块表将 `ErrorClassifier` 放在 retry 模块，实际在 errors 模块
- `transport/mod.rs` docstring 将 deprecated `denormalize_stream_chunk` 列为活跃方法
- `lib.rs:22-28` `resolve()` 声称 "stateless"，OnceLock 使 catalog 失败永久化
- `dispatcher.rs` docstring 不提及 OpenAICompatTransport
- `AnthropicTransport` 未从 mod.rs re-export