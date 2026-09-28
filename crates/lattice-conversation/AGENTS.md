# lattice-conversation 工作指引

## 职责

维护 provider-neutral 纯数据对话：有序 `NativeMessage<ToolCallState>`/`MessagePart`、`UnboundToolCall`/`BoundToolCall` typestate、闭合的 `ToolResultOutcome` 与结构化结果载荷、双摘要、attempt usage/中断/拒绝、provider origin 与 part-local replay state，以及 `ConversationItem`。

## 上位文档

修改前读取[会话对话模型](../../docs/architecture/session.md)、[LANCET 调用身份](../../docs/architecture/lancet.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 仅依赖 `lancet-protocol` 和中立数据依赖；不引入 gix、文件系统、store、锁、runtime、HTTP 客户端或 ID 生成器。InvocationId/TurnId 生成归 API 协调层。
- 使用绑定 typestate，不以 `Option<InvocationId>` 代替。Tool 参数必须是一个 JSON object，不把空串规范化为 `{}`；一个无效调用拒绝整批。
- 完整 provider 批次须通过 `validate_tool_call_batch`，公开上限 `MAX_TOOL_CALLS_PER_BATCH = 32`，超限拒绝整次 attempt，不截断；会话库登记时再次核验批次与绑定。
- 参数摘要为 SHA-256(domain separator + u64_be 字节长度 + canonical bytes)。BoundToolCall 反序列化时从参数重算摘要，不一致报 `ArgumentsDigestMismatch`。
- semantic 与 replay 摘要保持分离，字段范围遵循会话合同。绑定摘要使用原样存储且含 reasoning 的消息，不使用会丢弃 reasoning/replay 的 `project_cross_provider` 输出。
- replay state 按 part 原样保留，上限 64 KiB；即使 part 其他内容为空也不删。`Option<TokenUsage>` 缺失表示未知，不当作零。
- 所有完整 attempt 组装，包括 `lattice-llm::provider_projection`，均经 `AttemptAssembly` 持续计量，不先组装再检查。

## 验证入口

`cargo test -p lattice-conversation`。摘要或构造变化核对 `src/digest.rs` golden fixtures；golden 值变化是 wire-breaking 事件，不能机械更新期望值消除失败。
