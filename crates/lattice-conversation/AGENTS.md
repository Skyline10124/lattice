# lattice-conversation 工作指引

定义 provider 中立的纯数据对话类型、模型选择值、工具调用绑定 typestate 与字节计量规则，被 `lattice-session`、`lattice-profile`、`lattice-llm`、`lattice-agent` 与 `lattice-api` 使用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出对话类型、协议上限常量，以及 `lancet-protocol` 的调用身份、地址、`canonical_json` 与 `arguments_digest`，`lattice-*` 经此导入这些项 |
| `src/effort.rs` | 推理档位 `Effort` 解析与排序 |
| `src/model_selection.rs` | `ProviderId`、`ModelId`、`ModelRef` 构造与字符、长度校验 |
| `src/message.rs` | 有序 `NativeMessage<ToolCallState>`、`MessagePart`、回放来源 `ReplayOrigin` 与 64 KiB `OpaqueProviderState` |
| `src/tool_call.rs` | `UnboundToolCall` / `BoundToolCall` typestate、规范 JSON 参数对象、域分离 SHA-256 摘要与 `validate_tool_call_batch` |
| `src/limits.rs` | 流式增量计量 `AttemptAssembly`、完整回复边界核验 `validate_assistant_bounds` 与字节上限常量 |
| `src/outcome.rs` | 闭合的调用结局 `ToolResultOutcome` 与 `IndeterminateCause` |
| `src/result.rs` | 结构化工具结果 `StoredToolResult`、`ResultPayload` 与 64 KiB 正文保存边界 |
| `src/attempt.rs` | 模型尝试中断与批次拒绝记录（`AttemptInterruption`、`AttemptRejection`） |
| `src/usage.rs` | 尝试级 `TokenUsage`、`UsageCompleteness`、费用结果数据 `CostEstimate` 与汇总 `UsageSummary` |
| `src/history.rs` | provider 中立对话历史项 `ConversationItem` |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [对话数据与模型投影](../../docs/architecture/session/conversation.md)、[工程规范](../../docs/development/engineering.md) |
| 工具调用批次、绑定与结果边界 | [LANCET 工具执行](../../docs/architecture/lancet.md#登记与持久边界)、[Step 执行与结果保存](../../docs/architecture/session/steps.md#调用槽位) |
| 回复字节计量与上限常量 | [Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#模型请求与执行预算) |
| 用量、完整性与费用数据结构 | [用量、缓存与费用](../../docs/architecture/llm/usage.md#用量换算)、[关闭 Step 与 Turn](../../docs/architecture/session/steps.md#关闭-step-与-turn) |

## 约束

- `Cargo.toml` 仅依赖 `lancet-protocol` 与纯数据序列化、摘要库，保持确定性的内存数据结构与校验逻辑。
- 工具调用以 `UnboundToolCall` 与 `BoundToolCall` typestate 区分待登记与已绑定状态；参数在构造时解析为单个 JSON 对象，完整批次经 `validate_tool_call_batch` 核验（单批上限 `MAX_TOOL_CALLS_PER_BATCH = 32`，任一无效或超限则整批拒绝），保证进入会话库的调用槽位均对应完整有效回复，见[有序消息与 ToolCall](../../docs/architecture/session/conversation.md#有序消息与-toolcall)。
- 参数摘要按 `SHA-256(domain_separator + u64_be(len) + canonical_bytes)` 计算，`BoundToolCall` 反序列化时从参数重算摘要并在失配时返回 `ArgumentsDigestMismatch`；`src/tool_call.rs` 的 golden 摘要值锁定持久与 wire 兼容性，变更该算法前须由维护者确认迁移方案。
- `OpaqueProviderState` 按片段原样保留不透明回放材料（上限 64 KiB，包含正文为空的片段），见[回放与跨 provider 投影](../../docs/architecture/session/conversation.md#回放与跨-provider-投影)。
- `Option<TokenUsage>` 为 `None` 表示整次尝试缺失用量，字段级 `None` 表示该项未报告；完整性由 `UsageCompleteness` 表达，路径与全会话汇总按[关闭 Step 与 Turn](../../docs/architecture/session/steps.md#关闭-step-与-turn)派生。
- `AttemptAssembly` 与 `validate_assistant_bounds` 共用同一套字节上限常量和计量规则，使流式阶段与会话库持久边界的检查口径一致，见[模型请求与执行预算](../../docs/architecture/session/acceptance.md#模型请求与执行预算)。
- 本 crate 之外：流式回复组装、provider 协议投影与费用计价逻辑由 `lattice-llm` 负责；`TurnId` 与 `InvocationId` 生成由 `lattice-api` 负责；持久落盘与锁由 `lattice-session` 和 `lattice-store` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-conversation` |
| 修改参数规范化或摘要构造 | 运行 `cargo test -p lattice-conversation`，保持 `src/tool_call.rs` 的 golden 摘要用例原值通过 |
| 修改公开对话类型、上限常量或用量结构 | `cargo test -p lattice-session`、`cargo test -p lattice-llm`、`cargo test -p lattice-api` |
