# LATTICE 已解决问题归档

**最后更新**: 2026-05-01

---

## 归档说明

本文档记录所有已解决并关闭的问题。每个条目包含问题描述、根因分析、解决方案、验证结果和解决日期。

活跃问题见 [active-issues.md](active-issues.md)。

---

## 第一轮审查修复（2026-04-28 → 2026-04-29）

审查基线：44 个问题（2 致命 / 14 高优 / 16 中优 / 12 低优）

### A1：删除死代码 providers/ 模块（~3010 行）

| 字段 | 值 |
|------|-----|
| **ID** | A1 |
| **等级** | P0 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `28493f9` |

**描述**：`providers/` 模块包含 ~3010 行未使用代码，包括旧的 Provider 实现和 mock 数据。

**根因**：从 monolithic 架构迁移到分层 workspace 时，旧 provider 实现未清理。

**解决方案**：删除整个 `providers/` 目录。

**验证**：`cargo test` 全部通过，无编译错误。

---

### A2：删除 mock.rs

| 字段 | 值 |
|------|-----|
| **ID** | A2 |
| **等级** | P0 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `72cb8f2` |

**描述**：`mock.rs` 包含硬编码的测试 mock，不再被任何代码引用。

**根因**：测试重构后 mock 被替换，但文件未删除。

**解决方案**：删除文件。

**验证**：编译通过，无引用残留。

---

### A3：删除 Provider trait / ProviderError / async-trait

| 字段 | 值 |
|------|-----|
| **ID** | A3 |
| **等级** | P0 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `c3865fe` |

**描述**：`Provider` trait 及其错误类型已被 `Transport` trait 取代，但仍存在于代码中。

**根因**：架构迁移（Provider → Transport）后旧抽象未清理。

**解决方案**：删除 `Provider` trait、`ProviderError`、`async-trait` 依赖。

**验证**：编译通过，`grep -r "Provider trait"` 无结果。

---

### A4：删除过时的 Python 示例

| 字段 | 值 |
|------|-----|
| **ID** | A4 |
| **等级** | P0 |
| **组件** | lattice-python |
| **解决日期** | 2026-04-29 |
| **Commit** | `8bdbb5f` |

**描述**：Python 示例引用已删除的 API。

**根因**：API 重构后示例未同步更新。

**解决方案**：删除过时示例文件。

**验证**：Python binding 编译通过。

---

### A2*：移除 python crate 未使用依赖

| 字段 | 值 |
|------|-----|
| **ID** | A2* |
| **等级** | P0 |
| **组件** | lattice-python |
| **解决日期** | 2026-04-29 |
| **Commit** | `ad9f4d1` |

**描述**：`lattice-python` 的 `Cargo.toml` 包含不再使用的依赖。

**根因**：功能裁剪后依赖未清理。

**解决方案**：清理未使用依赖。

**验证**：`cargo check -p lattice-python` 通过。

---

### H1：HTTPS 强制校验

| 字段 | 值 |
|------|-----|
| **ID** | H1 |
| **等级** | 高优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `390e6bb` |

**描述**：非 localhost 的 HTTP base_url 未被拒绝，可能导致 API key 明文传输。

**根因**：初始实现未考虑 HTTPS 强制。

**解决方案**：`validate_base_url()` 拒绝非 localhost HTTP URL。

**验证**：测试确认 HTTP URL 被拒绝，localhost HTTP 和所有 HTTPS 通过。

---

### M1：chat() 添加 tools 参数

| 字段 | 值 |
|------|-----|
| **ID** | M1 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `733a0bf` |

**描述**：`chat()` 不接受 tools 参数，无法传递 tool definitions 给 provider。

**根因**：初始签名设计遗漏。

**解决方案**：`chat()` 签名添加 `tools: Option<&[ToolDefinition]>`。

**验证**：OpenAI tool calling 端到端测试通过。

---

### M2：resolve() 文档说明

| 字段 | 值 |
|------|-----|
| **ID** | M2 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `f9424e0` |

**描述**：`resolve()` 函数缺少文档注释。

**根因**：开发时未添加 doc comment。

**解决方案**：添加详细 doc comment。

**验证**：`cargo doc` 无 missing docs warning。

---

### M3：Agent retry 逻辑激活

| 字段 | 值 |
|------|-----|
| **ID** | M3 |
| **等级** | 中优 |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-29 |
| **Commit** | `733a0bf` |

**描述**：`Agent` 的 retry 逻辑存在但未激活。

**根因**：retry 代码骨架已写但未接入调用链。

**解决方案**：接入 `RetryPolicy`，在 `chat_with_retry()` 中实现重试循环。

**验证**：retry 测试确认 429 触发重试。

---

### M4：HTTP 408/504 重试分类

| 字段 | 值 |
|------|-----|
| **ID** | M4 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `0a19139` |

**描述**：`ErrorClassifier` 未将 408/504 分类为可重试。

**根因**：初始分类只覆盖 429/5xx，遗漏 408/504。

**解决方案**：添加 408/504 到 retryable status 列表。

**验证**：`ErrorClassifier` 单元测试覆盖 408/504。

---

### M5：extract_retry_after 字符串值

| 字段 | 值 |
|------|-----|
| **ID** | M5 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `f742ca9` |

**描述**：`retry-after` header 值可能是字符串格式的时间戳，解析只处理整数。

**根因**：HTTP spec 允许 `Retry-After` 为 HTTP-date 格式。

**解决方案**：添加字符串格式解析支持。

**验证**：整数和字符串格式均正确解析。

---

### M6：ContextWindowExceeded 提取 token 值

| 字段 | 值 |
|------|-----|
| **ID** | M6 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `d02c3db` |

**描述**：`ContextWindowExceeded` 错误未提取 token 数量信息。

**根因**：错误构造时未解析响应体中的 token 字段。

**解决方案**：从错误响应体解析 token 值。

**验证**：`ContextWindowExceeded` 包含 token 数量字段。

---

### M7：truncate_body UTF-8 安全截断

| 字段 | 值 |
|------|-----|
| **ID** | M7 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `0e2a125` |

**描述**：错误响应体截断可能切在 UTF-8 多字节字符中间。

**根因**：`&body[..limit]` 不检查 char boundary。

**解决方案**：使用 `is_char_boundary()` 安全截断。

**验证**：中文/emoji 内容截断测试通过。

---

### M8：Tool 结果大小限制

| 字段 | 值 |
|------|-----|
| **ID** | M8 |
| **等级** | 中优 |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-29 |
| **Commit** | `0ea0dc8` |

**描述**：tool 执行结果无大小限制，可能导致内存溢出。

**根因**：初始实现未考虑 LLM 输入 token 限制。

**解决方案**：添加 1MB 默认限制 + `max_write_size` 配置。

**验证**：超限结果被截断，不 panic。

---

### M9：normalize_model_id 减少分配

| 字段 | 值 |
|------|-----|
| **ID** | M9 |
| **等级** | 中优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `15652d9` |

**描述**：`normalize_model_id` 多次调用 `to_lowercase()` 产生不必要的字符串分配。

**根因**：代码未优化热路径分配。

**解决方案**：合并为单次调用。

**验证**：`cargo test` 通过，无行为变化。

---

## 第二轮审查修复（2026-04-29）

### P0-2：lattice-core/pyproject.toml 遗留

| 字段 | 值 |
|------|-----|
| **ID** | P0-2 |
| **等级** | P0 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `9e7250a` |

**描述**：`lattice-core` 目录下遗留 Python 项目文件，PyO3 已迁移到 `lattice-python` crate。

**根因**：crate 拆分时文件未跟随迁移。

**解决方案**：删除文件。

**验证**：文件已不存在，`cargo check --workspace` 通过。

---

### P0-3：catalog base_url 未回退 provider_defaults

| 字段 | 值 |
|------|-----|
| **ID** | P0-3 |
| **等级** | P0 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `1796205` |

**描述**：`resolve()` 在 catalog entry 无 `base_url` 时未回退到 `provider_defaults` 表。

**根因**：`resolve_base_url()` 未覆盖所有调用点。

**解决方案**：`resolve_base_url()` 统一处理所有 base_url 来源。

**验证**：所有 provider 的 base_url 正确解析。

---

### P1-1：OpenAI [DONE] 覆盖真实 finish_reason

| 字段 | 值 |
|------|-----|
| **ID** | P1-1 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `d35c5fa` |

**描述**：OpenAI SSE 流末尾的 `[DONE]` 事件覆盖了前一个有效事件的 `finish_reason`。

**根因**：`[DONE]` 事件处理逻辑未区分"新数据"和"流结束信号"。

**解决方案**：`[DONE]` 事件返回空 vec，不覆盖已有 finish_reason。

**验证**：SSE 解析测试确认 `[DONE]` 不覆盖 finish_reason。

---

### P1-2：Anthropic streaming finish_reason 未映射

| 字段 | 值 |
|------|-----|
| **ID** | P1-2 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `d35c5fa` |

**描述**：Anthropic 的 `end_turn` / `tool_use` / `max_tokens` 未映射为 OpenAI 兼容的 `stop` / `tool_calls` / `length`。

**根因**：SSE 解析器直接透传 provider 原始值。

**解决方案**：`SseParser` 添加 `map_stop_reason()` 函数。

**验证**：`end_turn→stop`、`tool_use→tool_calls`、`max_tokens→length` 映射测试通过。

---

### P1-5：HTTP 30s timeout 杀长流式响应

| 字段 | 值 |
|------|-----|
| **ID** | P1-5 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `ad7684d` |

**描述**：默认 30s HTTP timeout 会中断长时间 streaming 响应。

**根因**：全局 read timeout 不适用于 SSE 长连接。

**解决方案**：只保留 `connect_timeout`，移除全局 read timeout。

**验证**：长 streaming 响应不再被超时中断。

---

### P1-N1：denormalize_stream_chunk vs SseParser 分歧

| 字段 | 值 |
|------|-----|
| **ID** | P1-N1 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `2551876` |

**描述**：两条流式解析路径（`denormalize_stream_chunk` 和 `SseParser`）产生不同结果。

**根因**：架构迁移期两套实现并存。

**解决方案**：trait 方法标 `#[deprecated]`，文档标注"主路径不使用此方法"。

**验证**：主路径使用 `SseParser`，deprecated 方法保留但标注。

---

### P1-N2：chat() 不注入 extra_headers

| 字段 | 值 |
|------|-----|
| **ID** | P1-N2 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `b68d96f` |

**描述**：`chat()` 不读取 `resolved.provider_specific` 中的 header 配置。

**根因**：`provider_specific` 字典解析逻辑未实现。

**解决方案**：遍历 `provider_specific` 注入 `header:` 前缀的键值对。

**验证**：自定义 header 正确注入 HTTP 请求。

---

### P1-N4：Stream ended 丢失 finish_reason

| 字段 | 值 |
|------|-----|
| **ID** | P1-N4 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `4daacc4` |

**描述**：`Stream ended` 事件不携带 finish_reason，默认值误导。

**根因**：stream 结束时未保留前序事件的 finish_reason。

**解决方案**：默认值改为 `"unknown"`，注释说明行为。

**验证**：stream 结束事件不再返回错误的 `"stop"`。

---

### P1-N5：Anthropic/Gemini denormalize_response 丢弃 model/usage

| 字段 | 值 |
|------|-----|
| **ID** | P1-N5 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `bf02b41` |

**描述**：`denormalize_response` 未从响应体提取 model 和 usage 字段。

**根因**：denormalize 逻辑只关注 content，未提取 metadata。

**解决方案**：从 response 提取 model 和 usage。

**验证**：`denormalize_response` 返回值包含 model 和 usage。

---

### P1-1b：多 System 消息覆盖而非合并

| 字段 | 值 |
|------|-----|
| **ID** | P1-1b |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `d905928` |

**描述**：多个 System 消息只保留最后一个，丢失前面的指令。

**根因**：消息处理逻辑用赋值而非追加。

**解决方案**：用 `\n\n` 合并多个 System 消息。

**验证**：多 system 消息合并测试通过。

---

### P2-2：credential cache key 粒度不足

| 字段 | 值 |
|------|-----|
| **ID** | P2-2 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `a56b612` |

**描述**：credential cache key 只用 provider name，不同 credential_keys 配置共享缓存。

**根因**：cache key 设计未考虑多 credential 场景。

**解决方案**：key 包含 credential_keys fingerprint。

**验证**：不同 credential 配置产生不同 cache key。

---

### P2-5：Agent tool result 缺 name

| 字段 | 值 |
|------|-----|
| **ID** | P2-5 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-29 |
| **Commit** | `fb35061` |

**描述**：tool result 只包含 content，缺少对应的 tool name。

**根因**：`AgentState` 只存 id→content 映射，未存 id→name。

**解决方案**：`AgentState` 维护 id→name 映射。

**验证**：tool result 包含 tool name。

---

### P2-6：UTF-8 截断 panic（push_tool_result）

| 字段 | 值 |
|------|-----|
| **ID** | P2-6 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-29 |
| **Commit** | `9d92282` |

**描述**：`push_tool_result` 中 `&content[..limit]` 可能切在 UTF-8 多字节字符中间。

**根因**：与 P1-3 同类 bug，byte index 不检查 char boundary。

**解决方案**：使用 `is_char_boundary()` 安全截断。

**验证**：中文/emoji 内容截断测试通过。

**注意**：同一 bug 在 `Agent::run()` 中仍存在（见活跃问题 P1-3）。

---

### P2-7：CI workflow 位置不对

| 字段 | 值 |
|------|-----|
| **ID** | P2-7 |
| **等级** | P2 |
| **组件** | CI |
| **解决日期** | 2026-04-29 |
| **Commit** | `58b9610` |

**描述**：GitHub Actions workflow 在子目录而非 workspace root。

**根因**：初始项目结构配置错误。

**解决方案**：移至 workspace root。

**验证**：CI 正确触发。

---

### P2-N5：is_credentialless 未知 provider 返回 true

| 字段 | 值 |
|------|-----|
| **ID** | P2-N5 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `7e72293` |

**描述**：未知 provider 被当作 credentialless，可能跳过认证。

**根因**：default 分支返回 true，应为 false。

**解决方案**：未知 provider 返回 false（需认证）。

**验证**：未知 provider resolve 失败而非静默跳过认证。

---

### P2-N6：credential cache 无公开 clear 方法

| 字段 | 值 |
|------|-----|
| **ID** | P2-N6 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `a283194` |

**描述**：`ModelRouter` 的 credential cache 无公开清理接口。

**根因**：只实现了内部方法，未暴露公开 API。

**解决方案**：暴露 `pub fn clear_credential_cache()`。

**验证**：外部 crate 可调用 `clear_credential_cache()`。

---

### P2-8：DeepSeek thinking 硬编码 canonical_id

| 字段 | 值 |
|------|-----|
| **ID** | P2-8 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-29 |
| **Commit** | `914ecbc` |

**描述**：DeepSeek thinking 模式硬编码了特定 canonical_id，无法匹配其他变体。

**根因**：匹配逻辑写死特定 ID 而非模式匹配。

**解决方案**：改为匹配 `api_model_id`。

**验证**：DeepSeek thinking 变体正确匹配。

---

## Gstack 审查修复（2026-04-30）

### C1：Clippy identity_op in sandbox.rs

| 字段 | 值 |
|------|-----|
| **ID** | C1 |
| **等级** | CI Blocker |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：`1 * 1024 * 1024` 触发 clippy identity_op 错误。

**根因**：`1 *` 是恒等操作。

**解决方案**：改为 `1024 * 1024`。

**验证**：`cargo clippy` clean。

---

### C2-C4：E2E 测试期望过时

| 字段 | 值 |
|------|-----|
| **ID** | C2, C3, C4 |
| **等级** | CI Blocker |
| **组件** | lattice-core (e2e tests) |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：3 个 e2e 测试的期望值在 provider-priority 重构后过时。

**根因**：router 行为变更后测试未同步更新。

**解决方案**：更新测试期望匹配当前 router 行为。

**验证**：`cargo test` 全绿。

---

### M11：Agent 每次创建新 tokio runtime

| 字段 | 值 |
|------|-----|
| **ID** | M11 |
| **等级** | 中优 |
| **组件** | lattice-agent |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：`Agent::send_message()` 每次调用创建新 tokio runtime。

**根因**：同步 API 需要桥接 async，初始实现用 `Runtime::new()`。

**解决方案**：引入 `SHARED_RUNTIME` + `run_async()` with `spawn_blocking` fallback。

**验证**：多次调用 `send_message()` 不创建新 runtime。

---

### L1：Anthropic SSE 解析器不改写 finish reason

| 字段 | 值 |
|------|-----|
| **ID** | L1 |
| **等级** | 低优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：Anthropic 的 `end_turn` / `tool_use` / `max_tokens` 未映射为标准值。

**根因**：与 P1-2 同源，SSE 解析器透传原始值。

**解决方案**：`map_stop_reason()` 翻译 `end_turn→stop`、`tool_use→tool_calls`、`max_tokens→length`。

**验证**：Anthropic streaming finish_reason 正确映射。

---

### L6：model_id.to_lowercase() 两次调用

| 字段 | 值 |
|------|-----|
| **ID** | L6 |
| **等级** | 低优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：`resolve_alias` 中 `normalize_model_id` 被重复调用。

**根因**：调用链中两处都做了 normalize。

**解决方案**：只调用一次。

**验证**：`cargo test` 通过，无行为变化。

---

### L8：错误响应体无大小限制

| 字段 | 值 |
|------|-----|
| **ID** | L8 |
| **等级** | 低优 |
| **组件** | lattice-core |
| **解决日期** | 2026-04-30 |
| **Commit** | — |

**描述**：错误响应体读取无大小限制，可能导致 OOM。

**根因**：`resp.text()` 读取全部 body 无截断。

**解决方案**：`MAX_ERROR_BODY_LENGTH = 8192` + `truncate_body()`。

**验证**：超长错误响应被截断，不 OOM。

---

## 文档不一致修复（2026-04-29，7 项全部修复）

| ID | 描述 | Commit | 验证 |
|----|------|--------|------|
| D-1 | credentialless priority 文档写反 | `3fd6c8c` | 文档与代码一致 |
| D-2 | 测试计数不一致 | `3fec92b` | 计数与 `cargo test` 输出匹配 |
| D-3 | write_file 校验声称不存在 | `e949632` | 文档描述与实际行为一致 |
| D-4 | NormalizedResponse 死代码未标注 | `ed97785` | 已标注 `#[deprecated]` |
| D-5 | `_PROVIDER_CREDENTIALS` 前缀误导 | `1582165` | 改为无下划线命名 |
| D-6 | 两条流式路径分歧未提及 | `17ea2a2` | 文档已说明 deprecated 路径 |
| D-7 | Agent API with_memory/with_token_pool 未标注 | `c4b3720` | 文档已标注未接入状态 |

---

## 死代码/清理修复（3 项完成）

| ID | 描述 | Commit | 验证 |
|----|------|--------|------|
| C-1 | `NormalizedResponse` 死代码 | `5f58ea3` | 已删除，编译通过 |
| C-2 | `_PROVIDER_CREDENTIALS` 下划线命名 | `1582165` | 已改为规范命名 |
| C-3 | NaN/Inf temperature 静默 fallback | `622f2b5` | 已加 warn 日志 |


---
## Active Issues Fix Batch（2026-05-01）

修复基线：27 个问题（1 P0 / 5 P1 / 11 P2 / 10 P3）

### P0-1：Python API 暴露 chat_complete / Message / Role

| 字段 | 值 |
|------|-----|
| **ID** | P0-1 |
| **等级** | P0 |
| **组件** | lattice-python |
| **解决日期** | 2026-05-01 |
| **Commit** | `1d57aa4` |

**描述**：Python binding 只暴露 `resolve_model` / `list_models` / `list_authenticated_models`，没有 chat / streaming / tool calling。

**根因**：binding 实现只覆盖了 resolver API，chat 和 streaming 路径未暴露到 Python。

**解决方案**：添加 `chat_complete()` 方法，暴露 `Message`、`Role`、`ToolDefinition`、`ToolCall` 类型到 Python API。

**验证**：Python 端到端测试通过，`engine.chat_complete()` 返回正确结果。

---

### P1-1：ErrorClassifier 未贯通 streaming

| 字段 | 值 |
|------|-----|
| **ID** | P1-1 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `1d57aa4` |

**描述**：`ErrorClassifier` 可分类 429/401/5xx，但 `chat()` 的 SSE stream consumption 阶段完全绕过它。`StreamEvent::Error{message}` 丢失 status code / provider / retry-after / retryable 信息。

**根因**：SSE 连接错误发生在 response body 可用之前，HTTP status 未传递给 `ErrorClassifier`。

**解决方案**：从连接错误提取 HTTP status，在 `send_streaming_request()` 中调用 `ErrorClassifier::classify()` 分类后再包装错误。

**验证**：错误事件包含 status code、provider、retryable 标记；测试确认 401 正确分类为 AuthenticationError。

---

### P1-2：Gemini chat() 不支持

| 字段 | 值 |
|------|-----|
| **ID** | P1-2 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `1d57aa4` |

**描述**：`chat()` match 只有 `OpenAiChat` 和 `AnthropicMessages`。Gemini resolve 成功但 chat 返回 config error。

**根因**：GeminiGenerateContent protocol 未在 `chat()` 的分发逻辑中注册。

**解决方案**：实现 `GeminiGenerateContent` protocol handler，在 `chat()` 中添加 Gemini 分支调用对应的 transport。

**验证**：`resolve("gemini-pro")` + `chat(resolved, messages)` 成功返回流式响应。

---

### P1-3：Agent.run() UTF-8 截断 bug

| 字段 | 值 |
|------|-----|
| **ID** | P1-3 |
| **等级** | P1 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：memory auto-save 的 `&content[..200]` 可能切在多字节 UTF-8 字符中间，导致 panic。

**根因**：`format!("{}...", &content[..200])` 不检查 char boundary。`push_tool_result` 已用 `is_char_boundary` 修复，但 `run()` 中未修复。

**解决方案**：使用 `content.char_indices().take_while(|(i, _)| *i < 200).last()` 找到边界，与 `errors.rs` 中 `truncate_body` 同模式。

**验证**：中文/emoji 内容 > 200 字节不再 panic。

---

### P1-4：InMemoryMemory 全局状态导致测试污染

| 字段 | 值 |
|------|-----|
| **ID** | P1-4 |
| **等级** | P1 |
| **组件** | lattice-memory |
| **解决日期** | 2026-05-01 |
| **Commit** | `b185db6` |

**描述**：`InMemoryMemory` 使用 `LazyLock<Mutex<GlobalStore>>` 全局状态，测试间互相污染。

**根因**：`InMemoryMemory` struct 无字段，状态只能放全局。设计为 trivial stub，未考虑测试隔离。

**解决方案**：添加 `clear()` 方法清除全局存储，可在测试间调用清理。

**验证**：测试在 setup/teardown 中调用 `clear()`，各测试互不污染。

---

### P1-5：Gemini 随机 tool call ID 破坏幂等性

| 字段 | 值 |
|------|-----|
| **ID** | P1-5 |
| **等级** | P1 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `1d57aa4` |

**描述**：`denormalize_response` / `denormalize_stream_chunk` 每次调用生成随机 UUID 作为 tool call ID，同一响应两次调用产生不同 ID。

**根因**：Gemini API 不返回 tool call ID，transport 用 UUID 填充，随机 ID 无法映射到 tool result。

**解决方案**：使用 `generate_call_id()` 生成确定性工具 call ID（`format!("tc_{}", index)`），文档说明 Gemini 不支持 tool call ID 的限制。

**验证**：同一响应多次解析产生相同 tool call ID。

---

### P2-1：bash 工具命令注入风险

| 字段 | 值 |
|------|-----|
| **ID** | P2-1 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`DefaultToolExecutor` 的 `bash` / `run_command` 用 `starts_with()` 做 allowlist 检查，`"cargo test; rm -rf /"` 通过 `starts_with("cargo test")` 检查。

**根因**：sandbox 将命令视为字符串前缀匹配，而非分词验证命令结构。

**解决方案**：解析命令为 `(program, args)`，使用 program-based allowlist；拒绝含 `;`、`|`、`&&`、`||`、`$()`、反引号的原始命令。

**验证**：`"cargo test; rm -rf /"` 被拒绝，`"cargo test"` 正常执行。

---

### P2-2：resolve 无法区分 credentialless vs missing

| 字段 | 值 |
|------|-----|
| **ID** | P2-2 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：credential 缺失时 `resolve()` 返回 `api_key: None`，调用方无法区分"Ollama（不需要 key）" vs "Anthropic（没配 key）"。

**根因**：`ResolvedModel` 只存 `api_key: Option<String>`，缺少 credential 状态枚举。

**解决方案**：添加 `CredentialStatus` 枚举（`Present` / `NotRequired` / `Missing`），在 `ResolvedModel` 中增加 `credential_status` 字段。

**验证**：Ollama resolve 返回 `NotRequired`，未配置 API key 的 Anthropic 返回 `Missing`。

---

### P2-3：Agent memory / token_pool setter 存在但行为未接入

| 字段 | 值 |
|------|-----|
| **ID** | P2-3 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`with_memory()` / `with_token_pool()` 存在，但 `send_message()` / `run_chat()` 零引用 token_pool，memory 只在 `run()` 的 auto-save 用了一次。

**根因**：setter 接口已暴露但底层逻辑未完全接入。

**解决方案**：标记为 `#[deprecated]` 并添加文档注释说明当前接入状态和计划。

**验证**：编译警告提示用户当前接入状态。

---

### P2-4：Plugin extract_confidence() 解析失败返回 1.0

| 字段 | 值 |
|------|-----|
| **ID** | P2-4 |
| **等级** | P2 |
| **组件** | lattice-plugin |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`extract_confidence()` 在找不到 confidence 字段时默认返回 1.0，与 StrictBehavior 设计意图矛盾。

**根因**：默认值选择错误，解析失败应表示低置信度而非高置信度。

**解决方案**：默认值改为 0.0，解析失败 = 低置信度。

**验证**：无 confidence 字段的 response 返回 0.0。

---

### P2-5：Agent mid-stream 错误不重试

| 字段 | 值 |
|------|-----|
| **ID** | P2-5 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：`Agent::run()` 中 `run_chat()` 返回 `LoopEvent::Error` 时，循环继续下一轮而非重试。stream 开始后中途出错，部分响应被静默丢弃。

**根因**：retry 条件逻辑错误（`!has_error || !has_only_errors`），阻止了错误事件触发重试。

**解决方案**：retry 条件简化为检查 `has_error`；添加 `pop_last_assistant_message()` 撤销已推入的 assistant 消息后重试；在 `run_async()` 中添加相同重试逻辑。

**验证**：测试确认 streaming 错误后自动重试，不丢失上下文。

---

### P2-6：chat_with_retry 每次重试克隆全量消息

| 字段 | 值 |
|------|-----|
| **ID** | P2-6 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：每次重试 `clone()` 全部 messages + tools。长对话多工具时 O(n) per attempt。

**根因**：clone 调用在循环体内，未缓存。

**解决方案**：将 messages 和 tools 的 clone 缓存到循环外部，避免每次重试重复分配。

**验证**：重试路径不再产生额外分配，性能测试通过。

---

### P2-7：denormalize_stream_chunk 仍保留完整实现

| 字段 | 值 |
|------|-----|
| **ID** | P2-7 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：已标 `#[deprecated]` 但 Anthropic transport 保留 ~75 行完整实现 + 测试。

**根因**：deprecation 后未删除实现体，公开 API 有误导性。

**解决方案**：删除实现体，返回 `vec![]` 空 vec。

**验证**：调用 deprecated 方法返回空结果，编译 warning 提示使用者迁移。

---

### P2-8：Memory trait async_trait 对同步实现有不必要开销

| 字段 | 值 |
|------|-----|
| **ID** | P2-8 |
| **等级** | P2 |
| **组件** | lattice-memory |
| **解决日期** | 2026-05-01 |
| **Commit** | `b185db6` |

**描述**：`InMemoryMemory` 方法完全同步，但 `async_trait` 每次调用加 `Pin<Box<dyn Future>>` 分配。

**根因**：`Memory` trait 使用 `async_trait` 宏，即使同步实现也产生堆分配。

**解决方案**：移除 `async_trait` 宏，将 trait 方法改为返回 `impl Future<Output = ...>` 或使用关联类型。

**验证**：性能测试确认无额外堆分配；`InMemoryMemory` 方法保持同步。

---

### P2-9：chat() 包含过多协议特定 HTTP 逻辑

| 字段 | 值 |
|------|-----|
| **ID** | P2-9 |
| **等级** | P2 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：`chat()` 函数 ~155 行协议特定 HTTP 逻辑（URL 构造、header 注入、SSE 创建），应属于 `Transport` trait。

**根因**：HTTP 请求构建逻辑未下放到各 Transport 实现，`chat()` 需要知道 `ApiProtocol` 变体和 `provider_specific` 键。

**解决方案**：添加 `apply_auth_to_request()` 到 `Transport` trait（默认 `Bearer`，Gemini 覆盖为 `x-goog-api-key`）；移除 `send_gemini_nonstreaming_request` 中的重复代码；`chat()` 简化为协议分发。

**验证**：所有 provider 认证正确，测试全绿。

---

### P2-10：DefaultToolExecutor 混合工具定义与执行

| 字段 | 值 |
|------|-----|
| **ID** | P2-10 |
| **等级** | P2 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：`DefaultToolExecutor` 是 770 行 monolith，同时定义 17 个工具和执行它们。工具定义与执行未分离。

**根因**：初始实现将所有功能集中在单一模块。

**解决方案**：拆分为 `tool_definitions.rs`（工具定义 + `default_tool_definitions()` 函数）和 `tools.rs`（`DefaultToolExecutor` + `ToolExecutor` impl），支持 per-tool override。

**验证**：`cargo test` 全绿，`default_tool_definitions()` 返回 17 个工具定义。

---

### P2-11：Harness Pipeline 同步阻塞 + fork 用 OS 线程

| 字段 | 值 |
|------|-----|
| **ID** | P2-11 |
| **等级** | P2 |
| **组件** | lattice-harness |
| **解决日期** | 2026-05-01 |
| **Commit** | `b185db6` |

**描述**：`Pipeline::run()` 是同步阻塞调用，`run_fork()` 用 `std::thread::spawn`。在 async 上下文中阻塞 tokio runtime。

**根因**：初始实现未考虑 async 使用场景。

**解决方案**：添加 async 版本 `run_async()`，fork 用 `tokio::spawn` 替代 `std::thread::spawn`。

**验证**：`run_async()` 在 tokio runtime 中不阻塞工作线程；fork 任务正确并行执行。

---

### P3-1：Agent 零单元测试

| 字段 | 值 |
|------|-----|
| **ID** | P3-1 |
| **等级** | P3 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `840f4a6` |

**描述**：`lattice-agent` 只有 3 个测试（全是 `AgentState` 的），`Agent.run()` / `run_chat()` / 工具循环没有测试。

**根因**：开发初期测试覆盖不足。

**解决方案**：添加 mock HTTP server 集成测试，通过 `spawn_mock_server()` 模拟 SSE 响应，覆盖 `Agent::send_message()`、`Agent::run()`、工具循环路径。

**验证**：4 个集成测试覆盖主要 agent 路径，556 测试全绿。

---

### P3-2：grep 工具硬编码 --include=*.rs

| 字段 | 值 |
|------|-----|
| **ID** | P3-2 |
| **等级** | P3 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`DefaultToolExecutor` 的 grep 工具硬编码 `--include=*.rs`，只搜 Rust 文件。

**根因**：实现时未考虑通用性，直接硬编码了 Rust 文件过滤。

**解决方案**：移除硬编码的 `--include=*.rs`，让 grep 搜索所有文件类型。

**验证**：grep 工具能搜索 `.md`、`.toml` 等非 Rust 文件。

---

### P3-3：web_search / web_fetch 用 curl 命令行

| 字段 | 值 |
|------|-----|
| **ID** | P3-3 |
| **等级** | P3 |
| **组件** | lattice-agent |
| **解决日期** | 2026-05-01 |
| **Commit** | `b185db6` |

**描述**：项目已有 reqwest 依赖，但 web 工具用 `std::process::Command::new("curl")`。

**根因**：实现时未使用已有的 reqwest HTTP 客户端。

**解决方案**：使用 `reqwest` 替换 `curl` 子进程调用，移除 `curl` 作为外部依赖。

**验证**：`web_fetch` 和 `web_search` 通过 reqwest 正常返回结果。

---

### P3-4：resolve_permissive 不小写 model_part

| 字段 | 值 |
|------|-----|
| **ID** | P3-4 |
| **等级** | P3 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `—`（已在早期提交中修复） |

**描述**：`deepseek/DeepSeek-V4-Pro` 产生 `api_model_id: "DeepSeek-V4-Pro"` 而非 `"deepseek-v4-pro"`。

**根因**：`resolve_permissive` 中 `model_part` 未小写化。

**解决方案**：已在更早的迭代中通过 `model_lower` 变量修复，`resolve_permissive` 使用小写 model ID。

**验证**：`resolve("deepseek/DeepSeek-V4-Pro")` 返回 `api_model_id: "deepseek-v4-pro"`。

---

### P3-5：空 provider 列表 panic

| 字段 | 值 |
|------|-----|
| **ID** | P3-5 |
| **等级** | P3 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`sorted_providers[0]` 在 `entry.providers` 为空时 panic。

**根因**：未对空 provider 列表做 bounds check。

**解决方案**：在访问 `sorted_providers[0]` 前添加 bounds check，空列表时返回合适的错误。

**验证**：空 provider 列表的 entry 返回错误而非 panic。

---

### P3-6：Anthropic stop_reason "error" 未处理

| 字段 | 值 |
|------|-----|
| **ID** | P3-6 |
| **等级** | P3 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：`map_stop_reason` 对未知 reason 返回 `"stop"`，对 `"error"` 有误导性。

**根因**：match 分支未覆盖所有 Anthropic stop_reason 值。

**解决方案**：为 `"error"` stop_reason 添加映射；未知 reason 原样透传而非返回 `"stop"`。

**验证**：Anthropic `"error"` stop_reason 映射为正确值，未知 reason 原样传递。

---

### P3-7：ErrorClassifier 只检查一种 400 模式

| 字段 | 值 |
|------|-----|
| **ID** | P3-7 |
| **等级** | P3 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `e582b56` |

**描述**：只检查 `context_length_exceeded`，Anthropic 返回的 overloaded_error 等其他 400 错误类型未分类。

**根因**：`ErrorClassifier` 的 400 分支只匹配了一种模式。

**解决方案**：已通过 `overloaded_error` 模式扩展 400 分类覆盖范围，匹配更多 Anthropic 错误类型。

**验证**：`overloaded_error` 等新 400 模式被正确分类。

---

### P3-8：Plugin 只有 1 个用例，过度抽象

| 字段 | 值 |
|------|-----|
| **ID** | P3-8 |
| **等级** | P3 |
| **组件** | lattice-plugin |
| **解决日期** | 2026-05-01 |
| **Commit** | `a002a5a` |

**描述**：`lattice-plugin` 有 819 行代码、12 个测试，但只有 1 个实际用例。Plugin trait + Behavior enum + PluginRunner 抽象对单一用例过重。

**根因**：设计时预估了多个用例，但当前只有一个 CodeReviewPlugin 的实际需求。

**解决方案**：简化 `Behavior` 枚举，减少不必要的抽象层次。等待第二个用例出现后再固化抽象。

**验证**：plugin 编译通过，测试全绿，API 更简洁。

---

### P3-9：CLI/TUI 无测试

| 字段 | 值 |
|------|-----|
| **ID** | P3-9 |
| **等级** | P3 |
| **组件** | lattice-cli, lattice-tui |
| **解决日期** | 2026-05-01 |
| **Commit** | `840f4a6` |

**描述**：`lattice-cli`（1629 行）和 `lattice-tui`（815 行）编译通过但零测试。CLI 的 resolve / run / validate 等命令路径无覆盖。

**根因**：CLI/TUI 开发初期未建立测试。

**解决方案**：添加 CLI resolve / models 命令的 smoke 测试，验证基础命令路径可用。

**验证**：resolve / models smoke 测试通过，覆盖主要 CLI 入口。

---

### P3-10：无集成测试覆盖 resolve→chat→stream→chat_complete

| 字段 | 值 |
|------|-----|
| **ID** | P3-10 |
| **等级** | P3 |
| **组件** | lattice-core |
| **解决日期** | 2026-05-01 |
| **Commit** | `840f4a6` |

**描述**：无集成测试用 mock HTTP server 端到端验证 `resolve() → chat() → stream events → chat_complete()` 完整路径。

**根因**：e2e 测试依赖真实 API key，无法在 CI 中运行。

**解决方案**：创建 `chat_mock.rs` 测试模块，使用 mockito 模拟 HTTP server，验证完整推理路径不依赖真实 API key。

**验证**：mock 集成测试在 CI 中正常运行，覆盖 resolve→chat→stream→chat_complete 路径。

---

## 统计

| 批次 | 修复数 | 日期 |
|------|--------|------|
| 第一轮审查 | 15 | 2026-04-29 |
| 第二轮审查 | 22 | 2026-04-29 |
| Gstack 审查 | 7 | 2026-04-30 |
| 文档不一致 | 7 | 2026-04-29 |
| 死代码清理 | 3 | 2026-04-29 |
| Active Issues Fix Batch | 27 | 2026-05-01 |
| **总计** | **81** | |
