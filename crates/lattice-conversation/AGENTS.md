# lattice-conversation 工作指引

定义 provider 中立的纯数据对话类型、模型选择值、工具调用绑定 typestate 与字节计量规则，被 `lattice-session`、`lattice-profile`、`lattice-llm`、`lattice-agent` 与 `lattice-server` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出身份类型、结构地址、规范 JSON、参数摘要、对话类型与协议上限常量 |
| `src/identity.rs` | `SessionId`（32 位小写十六进制）、`TurnId` 与 `InvocationId`（小写连字符 UUID v4）严格解析、显示与 serde 表示，以及 `IdParseError` |
| `src/address.rs` | 结构化地址 `ModelMessageAddress`、`ToolCallAddress` 与模型尝试键 `ModelAttemptKey` |
| `src/canonical.rs` | 键按 UTF-8 字节序显式排序的无空白规范 JSON 序列化 `canonical_json` |
| `src/digest.rs` | 工具调用参数对象的域分离 SHA-256 摘要 `arguments_digest` |
| `src/effort.rs` | 推理档位 `Effort` 解析与排序 |
| `src/model_selection.rs` | `ProviderId`、`ModelId`、`ModelRef` 构造与字符、长度校验 |
| `src/message.rs` | 有序 `NativeMessage<ToolCallState>`、`MessagePart`、回放来源 `ReplayOrigin` 与 64 KiB `OpaqueProviderState` |
| `src/tool_call.rs` | `UnboundToolCall` / `BoundToolCall` typestate、规范 JSON 参数对象、域分离 SHA-256 摘要与 `validate_tool_call_batch` |
| `src/limits.rs` | 流式增量计量 `AttemptAssembly`、完整回复边界核验 `validate_assistant_bounds` 与字节上限常量 |
| `src/outcome.rs` | 闭合的调用结局 `ToolResultOutcome` 与 `IndeterminateCause` |
| `src/tool_error.rs` | 工具执行失败载荷 `ToolError { code, message }` |
| `src/result.rs` | 结构化工具结果 `StoredToolResult`、`ResultPayload`（含 `Failed(ToolError)`）与 64 KiB 正文保存边界 |
| `src/attempt.rs` | 模型尝试中断与批次拒绝记录（`AttemptInterruption`、`AttemptRejection`） |
| `src/usage.rs` | 尝试级 `TokenUsage`、`UsageCompleteness`、费用结果数据 `CostEstimate` 与汇总 `UsageSummary` |
| `src/history.rs` | provider 中立对话历史项 `ConversationItem` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [对话数据与模型投影](../../docs/architecture/session/conversation.md)、[工程规范](../../docs/development/engineering.md) |
| 工具调用批次、绑定与结果载荷 | [Step 执行与结果保存](../../docs/architecture/session/steps.md#调用槽位)、[ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md) |
| 回复字节计量与上限常量 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#模型请求与执行预算) |
| 用量、完整性与费用数据结构 | [用量、缓存与费用](../../docs/architecture/llm/usage.md#用量换算)、[关闭 Step 与 Turn](../../docs/architecture/session/steps.md#关闭-step-与-turn) |

## 约束

- `Cargo.toml` 仅依赖纯数据序列化、摘要与错误库，保持确定性的内存数据结构与校验逻辑。
- `tests/primitives_golden.rs` 锁定身份解析、显示、serde、`canonical_json` 与 `arguments_digest` 的逐字节行为，改动这些行为须由维护者确认。
- 工具调用以 `UnboundToolCall` 与 `BoundToolCall` typestate 区分待登记与已绑定状态；参数在构造时解析为单个 JSON 对象，完整批次经 `validate_tool_call_batch` 核验（单批上限 `MAX_TOOL_CALLS_PER_BATCH = 32`，任一无效或超限则整批拒绝），保证进入会话库的调用槽位均对应完整有效回复，见[有序消息与 ToolCall](../../docs/architecture/session/conversation.md#有序消息与-toolcall)。
- 参数摘要按 `SHA-256(domain_separator + u64_be(len) + canonical_bytes)` 计算，`BoundToolCall` 反序列化时从参数重算摘要并在失配时返回 `ArgumentsDigestMismatch`；`src/tool_call.rs` 的 golden 摘要值锁定持久与 wire 兼容性，变更该算法前须由维护者确认迁移方案。
- `OpaqueProviderState` 按片段原样保留不透明回放材料（上限 64 KiB，包含正文为空的片段），见[回放与跨 provider 投影](../../docs/architecture/session/conversation.md#回放与跨-provider-投影)。
- `Option<TokenUsage>` 为 `None` 表示整次尝试缺失用量，字段级 `None` 表示该项未报告；完整性由 `UsageCompleteness` 表达，路径与全会话汇总按[关闭 Step 与 Turn](../../docs/architecture/session/steps.md#关闭-step-与-turn)派生。
- `AttemptAssembly` 与 `validate_assistant_bounds` 共用同一套字节上限常量和计量规则，使流式阶段与会话库持久边界的检查口径一致，见[模型请求与执行预算](../../docs/architecture/session/acceptance.md#模型请求与执行预算)。
- 本 crate 之外：流式回复组装、provider 协议投影与费用计价逻辑由 `lattice-llm` 负责；`TurnId` 与 `InvocationId` 生成由 `lattice-server` 负责；持久落盘与锁由 `lattice-session` 和 `lattice-store` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-conversation` |
| 修改身份解析、结构地址、参数规范化或摘要构造 | 运行 `cargo test -p lattice-conversation`，保持 `tests/primitives_golden.rs` 与 `src/tool_call.rs` 的 golden 用例原值通过 |
| 修改公开对话类型、上限常量或用量结构 | `cargo test -p lattice-session`、`cargo test -p lattice-llm`、`cargo test -p lattice-server` |
