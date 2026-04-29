# artemis 代码审查报告（第三轮）

**审查日期**: 2026-04-29
**审查范围**: 全部 5 个 crate（artemis-core, agent, memory, token-pool, python）
**对比基准**: 第二轮审查（2026-04-29）发现的 27 个问题

---

## 总体变化

| 指标 | 第二轮 | 第三轮 | 变化 |
|------|--------|--------|------|
| 高优问题 | 5 | 1 | -4 |
| 中优问题 | 10 | 11 | +1 |
| 低优问题 | 12 | 12 | 0 |
| **总计** | **27** | **24** | **-3** |

第二轮 5 个高优中已修复 4 个（主要是 agent_loop 的 O(n²)、conversation clone 等随内核拆分自然消除）。

---

## 一、高优问题 (High)

### H1. HTTPS 校验缺失，API key 可能明文传输

**文件**: `artemis-core/src/router.rs:430-457`, `artemis-python/src/engine.rs:14-43`

`validate_base_url` 函数存在但**从未在生产代码中被调用**。文档注释说 "HTTPS validation lives in the engine layer"，但 engine 层没有做任何 HTTPS 校验。用户可以注册 `http://` 的 base URL，API key 明文传输。

**修复**: 在路由层或 engine 层添加 HTTPS 强制校验，仅允许 localhost/127.0.0.1 的 HTTP。

---

## 二、中优问题 (Medium)

### M1. `chat()` 硬编码 `tools: vec![]`，工具调用完全不可用

**文件**: `artemis-core/src/lib.rs:63`

```rust
let request = ChatRequest {
    messages: messages.to_vec(),
    tools: vec![],  // 硬编码为空
    ...
};
```

`chat()` 无条件发送空工具列表。Agent 没有注册工具的机制，即使注册了也无法传递。模型永远收不到 tool definitions。

**修复**: `chat()` 需要 `tools` 参数。

### M2. `resolve()` 每次创建新 ModelRouter，丢失自定义模型注册

**文件**: `artemis-core/src/lib.rs:24-26`

```rust
pub fn resolve(model: &str) -> Result<ResolvedModel, ArtemisError> {
    ModelRouter::new().resolve(model, None)  // 每次新建，无状态
}
```

Python 侧的 `ArtemisEngine` 维护了带 `Mutex` 的 `ModelRouter`，允许 `register_model()`。但独立调用 `resolve()` 会忽略所有自定义注册。两条解析路径产生不同结果。

### M3. Agent `retry` 字段是死代码

**文件**: `artemis-agent/src/lib.rs:24, 66-78`

`retry: RetryPolicy` 字段和 `with_retry()` builder 方法存在但从未被使用。`run_chat()` 在 `chat()` 报错时立即返回 `Error`，不做任何重试。

### M4. HTTP 408/504 未被分类为重试

**文件**: `artemis-core/src/errors.rs:158`

408（Request Timeout）和 504（Gateway Timeout）穿透到通用分支，变成 `Network` 错误，不可重试。应加入 500/502/503 的重试分支。

### M5. `extract_retry_after` 对字符串编码数值失败

**文件**: `artemis-core/src/errors.rs:220`

```rust
// "retry_after": "30"（JSON 字符串，非数值）
// after_colon.trim() 结果是 '"30"'，开头是引号
// take_while 不匹配引号 → num_str 为空 → 解析失败
```

LiteLLM、OpenRouter 等网关可能返回字符串编码的 retry_after。

**修复**: `after_colon.trim().trim_matches('"')` 去除引号。

### M6. `ContextWindowExceeded` 总是报告 0 token

**文件**: `artemis-core/src/errors.rs:165-169`

```rust
ArtemisError::ContextWindowExceeded { tokens: 0, limit: 0 }
```

Provider 返回的 token 实际数值未提取。Python 调用者收到 `tokens=0, limit=0`，无诊断信息。

### M7. `truncate_body` 在多字节 UTF-8 边界处 panic（新发现）

**文件**: `artemis-core/src/errors.rs:197`

```rust
truncated.push_str(&s[..MAX_ERROR_BODY_LENGTH]);  // 8192 字节
```

若 8192 字节边界落在多字节 UTF-8 字符中间，`&s[..8192]` 会 panic。

**修复**: 使用 `s.char_indices()` 找到安全边界再切片。

### M8. Tool call 结果无大小限制（DoS 向量）

**文件**: `artemis-agent/src/lib.rs:54-58`, `artemis-agent/src/state.rs:42-50`

工具返回大体积数据（多 GB 数据库查询、递归目录列表）时，无限制地存入对话历史，每次 `send()` 都会重复 clone。

### M9. `normalize_model_id` 过度分配

**文件**: `artemis-core/src/router.rs:63-71`

三次 `trim_start_matches("...").to_string()` 即使无匹配也分配新 String。`regex` replace 的 `Cow<str>` 被 `.to_string()` 强制分配。

**修复**: 链式 `trim_start_matches` 在 `&str` 引用上操作，`Cow::into_owned()` 仅在匹配时分配。

### M10. `chat()` 每次创建新的 `TransportDispatcher`

**文件**: `artemis-core/src/lib.rs:74, 111`

无状态 dispatcher 应使用 `LazyLock<TransportDispatcher>` 静态变量，避免每次调用重新创建。

### M11. `Agent` 每次创建新的 tokio runtime

**文件**: `artemis-agent/src/lib.rs:27`

多个 Agent 实例 = 多个独立 runtime，造成 `N * cores` 线程竞争。应使用共享 `LazyLock<Runtime>` 或 `Handle::current()`。

---

## 三、低优问题 (Low)

| # | 文件:行 | 问题 |
|---|---------|------|
| L1 | `streaming.rs:322` | Anthropic SSE 解析器不改写 finish reason（transport 层改写但 SSE 路径不走） |
| L2 | `router.rs:306` | `resolve_alias` 中重复调用 `normalize_model_id` |
| L3 | `router.rs:356` | `resolve_permissive` 的 model_part 未小写归一化 |
| L4 | `router.rs:221` | 空 provider 列表时 `[0]` 会 panic |
| L5 | `streaming.rs:312-324` | Anthropic `message_delta` 不处理 `stop_reason: "error"` |
| L6 | `tokens.rs:8,18` | `model_id.to_lowercase()` 被调用两次 |
| L7 | `provider.rs:12` | 30s HTTP 超时会杀死长流式响应 |
| L8 | `providers/mod.rs:52-56` | 错误响应体无大小限制读取 |
| L9 | `Cargo.toml` ×2 | tokio `"full"` feature 包含未使用的子系统 |
| L10 | `streaming.rs:256-258` | Anthropic 缓存 token 统计未捕获 |
| L11 | `router.rs:299-301` | `resolve_alias` 文档注释拼接了不存在函数的残留行 |
| L12 | `docs/architecture.md` | 引用了已删除的模块和 `TransportType` |

---

## 四、架构问题

| # | 等级 | 问题 |
|---|------|------|
| A1 | P0 | `providers/` 模块全部 8 个 provider 都是死代码——`chat()` 用 `TransportDispatcher` 不走它们 |
| A2 | P0 | `artemis-python` 的 Cargo.toml 依赖了 `artemis-agent` 但从未 import |
| A3 | P1 | Python 绑定未注册 `Message`、`Role`、`ToolDefinition` 等核心类型 |
| A4 | P1 | 全部 6 个 Python 示例文件引用了不存在的 API（`set_model`、`run_conversation` 等） |
| A5 | P2 | `#[allow(dead_code)]` 在 `Agent` 和 `LoopEvent` 上——真要保留还是删 |

---

## 五、优先修复清单

**第一阶段**（立即可修）：
1. 给 `chat()` 加 `tools` 参数（M1）
2. 共享 `ModelRouter` 实例或给 `resolve()` 加 router 参数（M2）
3. 删除或激活死代码 `providers/`（A1）
4. 移除 `artemis-python` 中未使用的 `artemis-agent` 依赖（A2）
5. 用 `LazyLock` 替换 `TransportDispatcher::new()`（M10）

**第二阶段**（基础设施）：
6. 在 engine 或 router 层强制 HTTPS 校验（H1）
7. 让 `retry` 字段在 `run_chat` 中生效（M3）
8. 添加 tool call 结果大小限制（M8）
9. 修复 HTTP 408/504 重试分类（M4）

**第三阶段**（打磨）：
10. 修复 `extract_retry_after` 字符串编码（M5）
11. 从错误体中提取实际 token 数值（M6）
12. 修复 `truncate_body` UTF-8 panic（M7）
13. 共享 tokio runtime（M11）
14. 修复 `normalize_model_id` 分配（M9）
15. 低优项 L1-L12

---

*本报告由 Claude Code 自动代码审查生成。*
