# artemis-core 代码审查报告（第二轮）

**审查日期**: 2026-04-29
**审查范围**: `artemis-core/` 全部源文件，含 opencode 修复后的代码
**对比基准**: 第一轮审查（2026-04-28）发现的 44 个问题

---

## 修复质量评估

opencode 的 7 波修复（Wave 0 ~ Wave REG）系统地处理了第一轮审查中的大部分问题：

| 第一轮等级 | 数量 | 已修复 | 部分修复 | 仍然存在 | 新发现 |
|-----------|------|--------|---------|---------|--------|
| 致命 (Critical) | 2 | 2 (C1, C2) | 0 | 0 | 0 |
| 高 (High) | 14 | 11 | 1 (H9) | 2 (H3, H8) | 1 |
| 中 (Medium) | 16 | 12 | 1 (M14) | 3 | 7 |
| 低 (Low) | 12 | 7 | 0 | 5 | 7 |

**整体修复率**: 30/44（68%）已修复，5/44 部分修复或仍存，9/44 未修复（主要是低优项）

### 关键修复确认

| 原问题 | 状态 |
|--------|------|
| C1 双 Transport trait | 已合并为单一 trait，`chat_completions.rs` 为 re-export |
| C2 AgentLoop 无 tokio runtime | AgentLoop 已创建 tokio runtime（但引入新问题，见下文） |
| H1 credentialless 优先级 | 正确处理三向优先级权衡 |
| H2 对话历史丢失 | 完整历史保留，state 正确更新 |
| H4 双 ErrorClassifier | `retry.rs` 中的重复已删除 |
| H5 Anthropic SSE error 吞噬 | 显式 `"error"` 匹配分支 |
| H6 Regex 编译开销 | `LazyLock<Regex>` 静态变量 |
| H7 reqwest Client 每次创建 | `LazyLock<reqwest::Client>` 共享，10s connect / 30s 总超时 |
| H11 rig-core | 已从 Cargo.toml 删除 |
| H12 API key 明文 HTTP | `engine.rs` 中非 localhost HTTP 被拒绝 |
| H13 HTTP 超时 | 已配置 10s connect + 30s total |
| H14 ResolvedModel Debug 脱敏 | 自定义 `Debug` impl，`api_key` 显示 `"***"` |
| M1 Anthropic usage 统计 | input_tokens 从 message_start 正确捕获，total 正确计算 |
| M2 jittered_backoff overflow | `saturating_pow` |
| M3 fits_in_context panic | Catalog::get() 使用 OnceLock + 返回 Config 错误 |
| M12 ProviderConfig 死代码 | 已删除 |
| M13 TransportType 注册 | 已删除 |
| M15 线性扫描 | HashMap |
| M16 双重分配 | `resp.json()` 替代 `text()` + `from_str` |

---

## 一、高优先级问题 (High)

### H-N1. `run_with_fallback` 无视错误分类，对所有错误执行 fallback

**文件**: `src/agent_loop.rs:266-269`

```rust
let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
if !has_error {
    return events;
}
// 不管是什么错误，都进行 fallback——包括 401/403（不可能在下一个 provider 成功）
```

`_classifier` 参数以下划线前缀命名（有意未使用）。`is_retryable()` 从未被调用。`AuthenticationError`（401/403）也会触发 fallback，浪费 backoff 延迟 + 在所有 provider 上都失败。

**修复**: 调用 `_classifier.is_retryable()` 并在 fallback 前检查错误类型。让 `LoopEvent::Error` 携带结构化错误类型，而非裸字符串。

### H-N2. `trim_conversation` O(n²) + 可能破坏 tool call 配对

**文件**: `src/agent_loop.rs:23-63`

三个独立问题：
1. **`Vec::remove(0)` 在循环中**（第 61 行）— O(n) 移位，使裁剪变成 O(n²)
2. **`system_msgs.clone()` 和 `non_system_msgs.clone()` 每轮迭代都 clone**（第 52-53 行）— 不必要的分配
3. **可能分割 tool-call 配对** — 从前面移除消息时，`Assistant` + `tool_calls` 消息可能与其后续的 `Tool` 结果消息分离，产生畸形对话

**修复**: 使用 `VecDeque` 实现 O(1) pop_front。在裁剪前检查 tool-call 配对边界。跟踪累积 token 计数，减去已移除消息的 token，而非每次从头重新计算。

### H-N3. `conversation.clone()` 每轮循环仍会 clone

**文件**: `src/agent_loop.rs:139`

```rust
let request = ChatRequest::new(conversation.clone(), tools.clone(), resolved.clone());
```

整个 `Vec<Message>` 在每轮循环都被 clone，此外还有 `trim_conversation` 的额外 clone（见 H-N2）。对于 50+ 条消息的对话，每轮都是一次 O(n) 的深拷贝。

### 原问题仍存

### H-N4. AgentLoop 注入硬编码 "mock tool result"（原 H3）

**文件**: `src/agent_loop.rs:147-152`

```rust
conversation.push(Message {
    content: "mock tool result".to_string(),
```

未改变。`resume_with_tool_results()` 方法已添加（第 218-237 行），提供了正确路径，但默认循环仍使用 mock 结果。

### H-N5. `budget_tokens` 使用 `conversation.clone()` 无界增长（原 H8）

**文件**: `src/agent_loop.rs:137`

trimming 逻辑已添加（解决了一半问题——现在有了预算执行），但每次 clone 整个对话的开销未解决。

---

## 二、中优先级问题 (Medium)

### M-N1. `ToolDefinition::set_parameters` 静默吞噬无效 JSON

**文件**: `src/types.rs:167-171`

```rust
fn set_parameters(&mut self, params: String) {
    if let Ok(val) = serde_json::from_str(&params) {
        self.parameters = val;
    }
    // 静默忽略无效 JSON —— 无错误返回，无警告
}
```

`ToolDefinition::new()` 正确返回了 `PyResult`，但 `set_parameters` 仍然静默吞噬错误。调用 `td.set_parameters("{bad json}")` 无任何反馈。

### M-N2. `extract_retry_after` 对字符串编码数值失败

**文件**: `src/errors.rs:375-383`

```rust
let after_colon = after_key[colon_pos + 1..].trim();
// "retry_after": "30" → after_colon 以 '"' 开头
// take_while 不匹配 '"' → 产生空字符串 → 解析失败
```

大多数 provider 返回数值，但 LiteLLM/OpenRouter 等网关可能返回字符串。函数应在提取前去除前导 `"`。

### M-N3. HTTP 408 未被分类为可重试

**文件**: `src/errors.rs:295-337`

408（Request Timeout）穿透到 `_ =>` 通用分支，变成 `Network` 错误 → `is_retryable` 返回 false。408 在语义上接近 503/429（瞬时错误），应为可重试。

### M-N4. `ContextWindowExceeded` 总是报告 0 token

**文件**: `src/errors.rs:327-328`

```rust
ArtemisError::ContextWindowExceeded {
    tokens: 0,
    limit: 0,    // 无诊断信息
}
```

Python 调用者收到 `ContextWindowExceededError`，其中 `tokens=0`，`limit=0`。

### M-N5. Tool call result 无大小限制

**文件**: `src/engine.rs:247-318, 392-465`, `src/agent_loop.rs:218-237`

Python 工具返回 MB/GB 级数据时，将原样存入对话历史并序列化到 API 请求体。应设置可配置的大小上限（如每结果 1MB）并做截断。

### M-N6. `normalize_model_id` 分配最多 7 个中间 String

**文件**: `src/router.rs:55-75`

`.to_lowercase()`、`.split_once()`、三次 `.trim_start_matches().to_string()`、`RE_SUFFIX.replace()`、`RE_DOTS.replace_all()` — 全部独立分配。常见情况（如 `"gpt-4o"` 无 Bedrock 前缀）即使字符串未改变仍会分配 5-6 次。考虑使用 `Cow<str>` 或链式操作。

### M-N7. `resolve_permissive` 硬编码 `context_length: 131072`

**文件**: `src/router.rs:361`

不适用于小窗口模型或 Gemini 1M+ token 窗口。

### M-N8. AgentLoop 创建自己的 tokio runtime（嵌套 `block_on` 风险）

**文件**: `src/agent_loop.rs:98`, `src/engine.rs:164-165`

`AgentLoop` 和 `ArtemisEngine` 各自创建独立的 tokio runtime。如果在 engine 的 `block_on` 上下文内调用 AgentLoop，tokio 将 panic（"Cannot block the current thread from within a runtime"）。

---

## 三、低优先级问题 (Low)

| # | 文件:行 | 问题 |
|---|---------|------|
| L-N1 | `errors.rs:357` | `truncate_body` 字节索引在多字节 UTF-8 字符处会 panic（实际中较低，因错误体为 ASCII） |
| L-N2 | `streaming.rs:134` | OpenAI 解析器对空 keep-alive chunk 无防护，会产生不必要的 `SseError::Parse` |
| L-N3 | `engine.rs:261,271,407,416` | 错误消息引用私有 `run_once()` 而非公开的 `run_conversation()` |
| L-N4 | `tokens.rs:49-54` | Token 估算忽略 tool call JSON、tool_call_id、role framing token |
| L-N5 | `transport/anthropic.rs:148` | `denormalize_response` 始终设置 `model: String::new()` — 与其他 provider 不一致 |
| L-N6 | `engine.rs:646-662` | `validate_base_url` 接受非 HTTP scheme（虽 reqwest 会拒绝，但 validator 不应声称它们合法） |
| L-N7 | `router.rs:309-310` | `resolve_alias` 中重复的 `normalize_model_id` 调用 |
| L-N8 | `transport/anthropic.rs:274,299` | `denormalize_stream_chunk` 对 delta/stop 事件使用合成 ID（`idx_0`），与 content_block_start 中的真实 ID 不匹配 |
| L-N9 | `chat_completions.rs:63-79` | `extra_headers` 字段无 setter |
| L-N10 | `streaming_bridge.rs:83` | Rust `Iterator::next` clone 完整 `LoopEvent`，即使通常只消耗一次 |
| L-N11 | `lib.rs:1` | `#![allow(deprecated)]` 在无废弃项残留后仍存在 |
| L-N12 | `Cargo.toml:21-23` | 未使用依赖：`uuid`、`chrono`、`pyo3-async-runtimes` |

---

## 四、代码结构

### 改进确认

- **Transport trait 已统一**: 单一 trait 定义于 `transport/mod.rs:72-146`，`chat_completions.rs:23` 为 re-export
- **ErrorClassifier 已统一**: `retry.rs` 中重复已删除，仅 `errors.rs` 留存
- **死代码已清理**: `ProviderConfig`、`TransportType` 已移除
- **Provier 共享逻辑已提取**: `openai_compat_chat()` 处理所有 OpenAI 兼容 provider 的公共 HTTP 逻辑

### 剩余担忧

- **5 个 provider 文件仍高度重复**（H9 部分修复）: deepseek.rs、mistral.rs、groq.rs、xai.rs、ollama.rs 共享通过 `openai_compat_chat()` 的公共逻辑，但 ~1300 行样板 struct/trait impl 和测试仍然重复。一个 `macro_rules! openai_compat_provider` 可将每个 provider 减少到 ~15 行
- **agent_loop 仍在核心中**: 708 行 agent_loop.rs 仍在主 crate 中
- **未使用依赖**: `uuid`、`chrono`、`pyo3-async-runtimes` 仍存在于 Cargo.toml
- **架构文档略有不同步**: `docs/architecture.md:76` 引用了已移除的 `TransportType`

---

## 五、安全

### 确认修复

| 原问题 | 状态 |
|--------|------|
| H12 API key 明文 HTTP | `engine.rs:646-662` 拒绝非 localhost HTTP |
| H13 HTTP 超时 | 共享 Client 配置 10s connect + 30s total |
| H14 Debug 脱敏 | 自定义 `Debug` impl，`api_key` → `"***"` |

### 新发现

| # | 等级 | 问题 |
|---|------|------|
| S1 | M | Tool call 结果无大小限制（DoS 向量）— `engine.rs:247`, `agent_loop.rs:218` |
| S2 | L | `#![allow(deprecated)]` 可移除 — `lib.rs:1` |
| S3 | L | 未使用依赖：`uuid`、`chrono`、`pyo3-async-runtimes` — `Cargo.toml:21-23` |

---

## 六、对比汇总

| 指标 | 第一轮 | 第二轮 | 变化 |
|------|--------|--------|------|
| 致命问题 | 2 | 0 | -2 |
| 高优问题 | 14 | 5 | -9 |
| 中优问题 | 16 | 10 | -6 |
| 低优问题 | 12 | 12 | 0 |
| **总计** | **44** | **27** | **-17 (-39%)** |
| 回归测试 | 无 | **714 个** | +714 |

---

## 七、优先修复清单

### 立即修复（低成本）

1. 删除未使用依赖（`uuid`、`chrono`、`pyo3-async-runtimes`）
2. 移除 `lib.rs` 中的 `#![allow(deprecated)]`
3. 修复 `ToolDefinition::set_parameters` 使其返回错误
4. 将 HTTP 408 添加到可重试分类
5. 修复 `extract_retry_after` 使其支持字符串编码值
6. 从 `ContextWindowExceeded` 中提取实际 token 计数

### 第二轮（中等成本）

7. 修复 `run_with_fallback` 使其检查 `is_retryable()`（H-N1）
8. 修复 `trim_conversation` O(n²) + tool-call 配对（H-N2）
9. 添加 tool call 结果大小限制（S1）
10. 在 `normalize_model_id` 中使用 `Cow<str>` 减少分配（M-N6）

### 第三轮（架构）

11. 使用 macro 减少 5 个 provider 文件的样板代码（H9 收尾）
12. 将 agent_loop 分离到独立 crate
13. 修复 `denormalize_stream_chunk` 合成 ID（L-N8）

---

*本报告由 Claude Code 自动代码审查生成。*
