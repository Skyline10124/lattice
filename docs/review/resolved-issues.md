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

## 统计

| 批次 | 修复数 | 日期 |
|------|--------|------|
| 第一轮审查 | 15 | 2026-04-29 |
| 第二轮审查 | 22 | 2026-04-29 |
| Gstack 审查 | 7 | 2026-04-30 |
| 文档不一致 | 7 | 2026-04-29 |
| 死代码清理 | 3 | 2026-04-29 |
| **总计** | **54** | |
