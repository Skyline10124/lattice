# LATTICE 活跃问题追踪

**最后更新**: 2026-05-01
**CI 基线**: 532 passed / 4 ignored / 0 failed, clippy clean, fmt clean

---

## 严重度定义

| 等级 | 含义 | 响应要求 |
|------|------|---------|
| P0 | 对外可用性阻断 | 立即修复 |
| P1 | 运行时正确性缺陷 | 本迭代修复 |
| P2 | 设计/安全/维护问题 | 计划中 |
| P3 | 代码质量改进 | 延后 |

---

## P0：对外可用性阻断

### P0-1：Python API 只有 resolver

| 字段 | 值 |
|------|-----|
| **ID** | P0-1 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md) |
| **组件** | lattice-python |
| **文件** | `lattice-python/src/lib.rs` |
| **状态** | Open |

**描述**：Python binding 只暴露 `resolve_model` / `list_models` / `list_authenticated_models`，没有 chat / streaming / tool calling。README 和 quickstart 暗示完整能力，实际不可用。

**复现步骤**：
1. `pip install lattice-python`
2. 尝试 `engine.chat_complete(resolved, messages)`
3. AttributeError: module has no attribute `chat_complete`

**影响范围**：所有 Python 用户无法通过 binding 执行推理。

**建议修复**：
1. 短期：修 README，明确 Python 目前只支持 resolver
2. 中期：暴露 `Message` / `Role` / `ToolDefinition` / `chat_complete`
3. 长期：暴露 streaming iterator 和 Agent API

---

## P1：运行时正确性

### P1-1：ErrorClassifier 未贯通 streaming

| 字段 | 值 |
|------|-----|
| **ID** | P1-1 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md), [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N7) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/lib.rs:148-151` |
| **状态** | Open |

**描述**：`ErrorClassifier` 可分类 429/401/5xx，但 `chat()` 的 SSE stream consumption 阶段完全绕过它。`StreamEvent::Error{message}` 丢失 status code / provider / retry-after / retryable 信息。

**复现步骤**：
1. 设置无效 API key
2. 调用 `chat()` 触发 401
3. SSE 连接阶段的错误被包装为 `LatticeError::Network`，丢失分类信息
4. streaming 阶段的 retryable error 无法触发自动重试

**根因**：SSE 连接错误发生在 response body 可用之前，`ErrorClassifier::classify()` 需要 `status_code` + `response_body`，但 HTTP status 从连接错误中是可获取的。

**建议修复**：从连接错误提取 HTTP status，调用 `ErrorClassifier` 分类后再包装。

---

### P1-2：Gemini chat() 不支持

| 字段 | 值 |
|------|-----|
| **ID** | P1-2 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/lib.rs` (chat match) |
| **状态** | Open |

**描述**：`chat()` match 只有 `OpenAiChat` 和 `AnthropicMessages`。Gemini resolve 成功但 chat 返回 config error。

**复现步骤**：
1. `resolve("gemini-pro")` → 成功
2. `chat(resolved, messages)` → `LatticeError::Config`

**建议修复**：短期在 resolve 返回 Gemini 时加 warning metadata；中期实现 Gemini non-streaming 主链路。

---

### P1-3：Agent.run() UTF-8 截断 bug

| 字段 | 值 |
|------|-----|
| **ID** | P1-3 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N1) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/lib.rs:199-200` |
| **状态** | Open |

**描述**：memory auto-save 的 `&content[..200]` 可能切在多字节 UTF-8 字符中间，导致 panic。

**复现步骤**：
1. Agent 对话内容含中文/emoji
2. 内容长度 > 200 字节且第 200 字节在多字节字符中间
3. `format!("{}...", &content[..200])` → panic: byte index 200 is not a char boundary

**根因**：无 UTF-8 安全截断工具。`push_tool_result` 已用 `is_char_boundary` 修复，但 `run()` 中未修复。

**建议修复**：使用 `content.char_indices().take_while(|(i, _)| *i < 200).last()` 找到边界，与 `errors.rs` 中 `truncate_body` 同模式。

---

### P1-4：InMemoryMemory 全局状态导致测试污染

| 字段 | 值 |
|------|-----|
| **ID** | P1-4 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N3) |
| **组件** | lattice-memory |
| **文件** | `lattice-memory/src/lib.rs:104-155` |
| **状态** | Open |

**描述**：`InMemoryMemory` 使用 `LazyLock<Mutex<GlobalStore>>` 全局状态。所有实例共享同一存储。`clone_arc()` / `clone_box()` 返回新实例但仍写入同一全局。

**复现步骤**：
1. Test A: `InMemoryMemory::new().save("key", "value")`
2. Test B: `InMemoryMemory::new().search("key")` → 返回 Test A 的数据
3. 无 `clear()` 方法可在测试间清理

**根因**：`InMemoryMemory` struct 无字段，状态只能放全局。设计为 trivial stub，未考虑并发和隔离。

**建议修复**：将 `RwLock<Vec<MemoryEntry>>` 放入 struct 内部，移除 `GLOBAL_STORE`。

---

### P1-5：Gemini 随机 tool call ID 破坏幂等性

| 字段 | 值 |
|------|-----|
| **ID** | P1-5 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N6) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/transport/gemini.rs:254,355` |
| **状态** | Open |

**描述**：`denormalize_response` / `denormalize_stream_chunk` 每次调用生成随机 UUID 作为 tool call ID。同一响应两次调用产生不同 ID，Agent 无法将 tool result 映射回原始 call。

**根因**：Gemini API 不返回 tool call ID，transport 用 UUID 填充。Agent 用 `tool_call_id` 映射 tool result 到原始 call，随机 ID 无法关联。

**建议修复**：使用 `format!("tc_{}", index)` 作为确定性伪 ID，文档说明 Gemini 不支持 tool call ID 的限制。

---

## P2：设计、安全和维护

### P2-1：bash 工具命令注入风险

| 字段 | 值 |
|------|-----|
| **ID** | P2-1 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N2) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/tools.rs:478-493` |
| **状态** | Open |

**描述**：`DefaultToolExecutor` 的 `bash` / `run_command` 用 `starts_with()` 做 allowlist 检查。`"cargo test; rm -rf /"` 通过 `starts_with("cargo test")` 检查。

**复现步骤**：
1. 配置 allowlist: `["cargo test"]`
2. LLM 生成 tool call: `bash("cargo test; rm -rf /")`
3. `starts_with("cargo test")` → true → 命令执行

**根因**：sandbox 将命令视为字符串前缀匹配，而非分词验证命令结构。在 agent loop 中 model output 即 tool input，prompt injection → 恶意 tool call。

**建议修复**：解析命令为 `(program, args)`，检查 `program`；拒绝含 `;` `|` `&&` `||` `$()` 反引号的原始命令。

---

### P2-2：resolve 无法区分 credentialless vs missing

| 字段 | 值 |
|------|-----|
| **ID** | P2-2 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/router.rs` |
| **状态** | Open |

**描述**：credential 缺失时 `resolve()` 返回 `api_key: None`。调用方无法区分 "Ollama（不需要 key）" vs "Anthropic（没配 key）"。

**建议修复**：在 `ResolvedModel` 加 `credential_status` enum（`Present` / `NotRequired` / `Missing`）。

---

### P2-3：Agent memory / token_pool setter 存在但行为未接入

| 字段 | 值 |
|------|-----|
| **ID** | P2-3 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/lib.rs` |
| **状态** | Open |

**描述**：`with_memory()` / `with_token_pool()` 存在，但 `send_message()` / `run_chat()` 零引用 token_pool，memory 只在 `run()` 的 auto-save 用了一次。

**建议修复**：要么接通，要么删掉 setter 别误导用户。

---

### P2-4：Plugin extract_confidence() 解析失败返回 1.0

| 字段 | 值 |
|------|-----|
| **ID** | P2-4 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-plugin |
| **文件** | `lattice-plugin/src/lib.rs` |
| **状态** | Open |

**描述**：`extract_confidence()` 在找不到 confidence 字段时默认返回 1.0。与 StrictBehavior 设计意图矛盾——解析失败应等于低置信度。

**建议修复**：默认值改为 0.0。

---

### P2-5：Agent mid-stream 错误不重试

| 字段 | 值 |
|------|-----|
| **ID** | P2-5 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N8) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/lib.rs:152-195` |
| **状态** | Open |

**描述**：`Agent::run()` 中 `run_chat()` 返回 `LoopEvent::Error` 时，循环继续下一轮而非重试。`chat_with_retry()` 只重试初始连接——stream 开始后中途出错，部分响应被静默丢弃。

**建议修复**：为 mid-stream 错误添加重试逻辑，或至少将错误传播给调用方。

---

### P2-6：chat_with_retry 每次重试克隆全量消息

| 字段 | 值 |
|------|-----|
| **ID** | P2-6 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N9) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/lib.rs:354-358` |
| **状态** | Open |

**描述**：每次重试 `clone()` 全部 messages + tools。长对话多工具时 O(n) per attempt。

**建议修复**：在循环外缓存 clone，或通过 `run_async` 传引用。

---

### P2-7：denormalize_stream_chunk 仍保留完整实现

| 字段 | 值 |
|------|-----|
| **ID** | P2-7 |
| **首次报告** | 2026-04-29 |
| **来源** | [implementation-review-2026-04-29.md](implementation-review-2026-04-29.md), [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N10) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/transport/mod.rs:127-130` |
| **状态** | Open |

**描述**：已标 `#[deprecated]` 但 Anthropic transport 保留 ~75 行完整实现 + 测试。公开 API 有误导性。

**建议修复**：删除实现体，改为返回 `vec![]` 或 panic。

---

### P2-8：Memory trait async_trait 对同步实现有不必要开销

| 字段 | 值 |
|------|-----|
| **ID** | P2-8 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N11) |
| **组件** | lattice-memory |
| **文件** | `lattice-memory/src/lib.rs:51` |
| **状态** | Open |

**描述**：`InMemoryMemory` 方法完全同步，但 `async_trait` 每次调用加 `Pin<Box<dyn Future>>` 分配。

**建议修复**：考虑 `-> impl Future<Output = ...>` 或拆分 sync/async 变体。低优先级。

---

### P2-9：chat() 包含过多协议特定 HTTP 逻辑

| 字段 | 值 |
|------|-----|
| **ID** | P2-9 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (架构评估) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/lib.rs` |
| **状态** | Open |

**描述**：`chat()` 函数 ~155 行协议特定 HTTP 逻辑（URL 构造、header 注入、SSE 创建）。这些应属于 `Transport` trait——每个 transport 应知道如何构建和执行自己的 HTTP 请求。当前设计迫使 `chat()` 知道 `ApiProtocol` 变体和 `provider_specific` 键如 `"auth_type"` / `"header:"`。

**建议修复**：将 HTTP 请求构建逻辑下放到各 Transport 实现，`chat()` 只做协议分发和 SSE 解析。

---

### P2-10：DefaultToolExecutor 混合工具定义与执行

| 字段 | 值 |
|------|-----|
| **ID** | P2-10 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (架构评估) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/tools.rs` |
| **状态** | Open |

**描述**：`DefaultToolExecutor` 是 770 行 monolith，同时定义 17 个工具（tool definitions）和执行它们（tool execution）。工具定义应与执行分离，让调用方可以为单个工具提供自定义实现。

**建议修复**：拆分为 `ToolRegistry`（定义）和 `ToolExecutor`（执行），支持 per-tool override。

---

### P2-11：Harness Pipeline 同步阻塞 + fork 用 OS 线程

| 字段 | 值 |
|------|-----|
| **ID** | P2-11 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-harness |
| **文件** | `lattice-harness/src/pipeline.rs:329-337` |
| **状态** | Open |

**描述**：`Pipeline::run()` 是同步阻塞调用。`run_fork()` 用 `std::thread::spawn` 做 fork 并行，每个分支占一个 OS 线程。在 async 上下文中使用会阻塞 tokio runtime。

**建议修复**：提供 async 版本 `run_async()`，fork 用 `tokio::spawn` 替代 `std::thread::spawn`。

---

## P3：代码质量

### P3-1：Agent 零单元测试

| 字段 | 值 |
|------|-----|
| **ID** | P3-1 |
| **首次报告** | 2026-04-29 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-agent |
| **状态** | Open |

**描述**：`lattice-agent` 只有 3 个测试（全是 `AgentState` 的）。`Agent.run()` / `run_chat()` / 工具循环没有测试。

---

### P3-2：grep 工具硬编码 --include=*.rs

| 字段 | 值 |
|------|-----|
| **ID** | P3-2 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/tools.rs` |
| **状态** | Open |

**描述**：`DefaultToolExecutor` 的 grep 工具硬编码 `--include=*.rs`，只搜 Rust 文件。

---

### P3-3：web_search / web_fetch 用 curl 命令行

| 字段 | 值 |
|------|-----|
| **ID** | P3-3 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-agent |
| **文件** | `lattice-agent/src/tools.rs` |
| **状态** | Open |

**描述**：项目已有 reqwest 依赖，但 web 工具用 `std::process::Command::new("curl")`。`web_search` 名字暗示搜索实际只是 fetch URL。

---

### P3-4：resolve_permissive 不小写 model_part

| 字段 | 值 |
|------|-----|
| **ID** | P3-4 |
| **首次报告** | 2026-04-29 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N12) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/router.rs:438` |
| **状态** | Open |

**描述**：`deepseek/DeepSeek-V4-Pro` 产生 `api_model_id: "DeepSeek-V4-Pro"` 而非 `"deepseek-v4-pro"`，可能导致 API 错误。

---

### P3-5：空 provider 列表 panic

| 字段 | 值 |
|------|-----|
| **ID** | P3-5 |
| **首次报告** | 2026-04-29 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (carried L4) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/router.rs:255` |
| **状态** | Open |

**描述**：`sorted_providers[0]` 在 `entry.providers` 为空时 panic。

---

### P3-6：Anthropic stop_reason "error" 未处理

| 字段 | 值 |
|------|-----|
| **ID** | P3-6 |
| **首次报告** | 2026-04-29 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (carried L5) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/streaming.rs` |
| **状态** | Open |

**描述**：`map_stop_reason` 对未知 reason 返回 `"stop"`，对 `"error"` 有误导性。

---

### P3-7：ErrorClassifier 只检查一种 400 模式

| 字段 | 值 |
|------|-----|
| **ID** | P3-7 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (N14) |
| **组件** | lattice-core |
| **文件** | `lattice-core/src/errors.rs:164` |
| **状态** | Open |

**描述**：只检查 `context_length_exceeded`，Anthropic 返回的 overloaded 等其他 400 错误类型未分类。

---

### P3-8：Plugin 只有 1 个用例，过度抽象

| 字段 | 值 |
|------|-----|
| **ID** | P3-8 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-plugin |
| **文件** | `lattice-plugin/src/lib.rs` |
| **状态** | Open |

**描述**：`lattice-plugin` 有 819 行代码、12 个测试，但只有 1 个实际用例（CodeReviewPlugin）。Plugin trait + Behavior enum + PluginRunner 的抽象层次对单一用例过重。`extract_confidence()` 的默认值 bug（P2-4）也源于缺乏多用例验证。

**建议修复**：等待第二个用例出现后再固化抽象。当前可简化 Behavior enum。

---

### P3-9：CLI/TUI 无测试

| 字段 | 值 |
|------|-----|
| **ID** | P3-9 |
| **首次报告** | 2026-05-01 |
| **来源** | [code-review-2026-05-01.md](code-review-2026-05-01.md) |
| **组件** | lattice-cli, lattice-tui |
| **状态** | Open |

**描述**：`lattice-cli`（1629 行）和 `lattice-tui`（815 行）编译通过但零测试。CLI 的 resolve / run / validate 等命令路径无覆盖。

---

### P3-10：无集成测试覆盖 resolve→chat→stream→chat_complete

| 字段 | 值 |
|------|-----|
| **ID** | P3-10 |
| **首次报告** | 2026-04-30 |
| **来源** | [gstack-review-2026-04-30.md](gstack-review-2026-04-30.md) (测试评估) |
| **组件** | lattice-core |
| **状态** | Open |

**描述**：无集成测试用 mock HTTP server 端到端验证 `resolve() → chat() → stream events → chat_complete()` 完整路径。当前 e2e 测试依赖真实 API key，无法在 CI 中运行。

**建议修复**：用 `mockito` 或 `wiremock` 创建 mock HTTP server，验证完整推理路径。

---

## 统计

| 等级 | 数量 |
|------|------|
| P0 | 1 |
| P1 | 5 |
| P2 | 11 |
| P3 | 10 |
| **总计** | **27** |
