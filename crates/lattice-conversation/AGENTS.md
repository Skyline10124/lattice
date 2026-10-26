# lattice-conversation 工作指引

## 职责

维护 provider-neutral 纯数据对话：有序 `NativeMessage<ToolCallState>`/`MessagePart`、`UnboundToolCall`/`BoundToolCall` typestate、闭合的 `ToolResultOutcome` 与结构化结果载荷、参数与内容摘要、attempt usage/中断/拒绝、provider origin 与 part-local replay state，以及 `ConversationItem`。

## 上位文档

修改前读取[会话对话模型](../../docs/architecture/session.md)、[LANCET 调用身份](../../docs/architecture/lancet.md)及[测试工程](../../docs/development/testing.md)。

## 局部约束

- 仅依赖 `lancet-protocol` 和中立数据依赖；不引入 gix、文件系统、store、锁、runtime、HTTP 客户端或 ID 生成器。InvocationId/TurnId 生成归 API 协调层。
- 使用绑定 typestate，不以 `Option<InvocationId>` 代替。Tool 参数必须是一个 JSON object，不把空串规范化为 `{}`；一个无效调用拒绝整批。
- 完整 provider 批次须通过 `validate_tool_call_batch`，公开上限 `MAX_TOOL_CALLS_PER_BATCH = 32`，超限拒绝整次 attempt，不截断；会话库登记时再次核验批次与绑定。
- 参数摘要为 SHA-256(domain separator + u64_be 字节长度 + canonical bytes)。BoundToolCall 反序列化时从参数重算摘要，不一致报 `ArgumentsDigestMismatch`。
- replay state 按 part 原样保留，上限 64 KiB；即使 part 其他内容为空也不删。`Option<TokenUsage>` 缺失表示未知，不当作零。
- `AttemptAssembly` 提供按调用 index 的参数计量及共享总量算法；流式组装由 `lattice-llm` 的共同累积器独占。`validate_assistant_bounds` 用同一组常量与算法核对完整消息，供会话库在持久边界独立检查，参数按规范形式计量。

## 验证入口

- 运行 `cargo test -p lattice-conversation`。
- 改了参数摘要或构造方式时，核对 `src/tool_call.rs` 中的 golden fixture。golden 值变化意味着 wire 格式不兼容，不能为了让测试通过直接改期望值。
