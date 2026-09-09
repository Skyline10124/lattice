# lattice-llm 工作指引

## 职责

维护 provider 请求与流交互、工具 wire 类型、SSE、模型路由、endpoint 元数据、请求时 DNS pinning 及日志设置。持久消息与 usage 直接使用 `lattice-conversation` 类型，不负责 Session 图、profile 发现、Agent 循环或 LANCET 执行。

- `types.rs`：请求信封、工具定义、流事件与目标；`client.rs`：HTTP 行为和请求时安全。
- `providers/`：provider 信封、认证、SSE 累积；`provider_projection/` 是唯一生产消息 codec。
- `sse.rs`：流解析；router/endpoints 模块：模型与 provider 解析；`stub.rs`：测试客户端；`logging.rs`：日志初始化。
- 共享 URL/DNS/地址检查归 `net-guard`，client 将中立错误映射为 `LlmError`。

## 上位文档

修改前读取[会话的对话与模型请求合同](../../docs/architecture/session.md)、[测试工程](../../docs/development/testing.md)；Tool 网络路径由 [LANCET 合同](../../docs/architecture/lancet.md)另行维护，不能覆盖本层 provider 策略。

## 局部约束

- provider 协议差异留在 provider/client 层，高层只消费类型化 `LlmEvent`、`LlmError` 和模型元数据。
- 所有请求 builder 经 `request_messages_to_wire`，权威 MessageComplete 经 `wire_to_native`，usage 经 `wire_usage`；不新建 ToolCall/ToolResult codec。组装经 `AttemptAssembly`，超限返回 `ProjectionError::OutputLimitExceeded`，不返回部分消息。
- provider call ID 是可选不透明 wire 元数据，不是领域 Invocation 身份，不合成 `tc_*` 身份。same-provider 保留真实 ID、签名及 part 顺序；cross-provider 去掉 replay state、provider call ID 与 reasoning，不泄露来源 provider 状态。
- `validate_base_url` 的 DNS 检查仅尽力而为，请求时解析地址固定才是主要 SSRF 防线。localhost、127.0.0.1、::1 使用一致的 loopback 例外，其他私有/保留 IP literal 拒绝。
- HTTP 默认 `redirect::Policy::none()`；重新允许重定向必须逐跳重新执行地址与 pinning 检查，避免向未经验证的主机转送 body 和密钥。不能依赖首跳校验保护后续跳。
- DNS rebinding 防御结合连接地址固定和 `net-guard` 分类/后缀规则。扩大启发式或后缀范围须有测试，并如实报告未覆盖风险。
- 点号工具名 `a.b` 与 wire 名 `a__b` 的编解码只属于 provider 适配，目录名仍保持点号。
- 日志写 stderr，ANSI 取决于交互 stderr 与 `NO_COLOR`，不污染 JSONL stdout。保留 `DEFAULT_MAX_OUTPUT_TOKENS` 默认来源，不重新硬编码旧 4096 上限；JSON 解析不以哨兵错误值承担正常控制流。

## 验证入口

- `cargo test -p lattice-llm`；projection 对照 `tests/fixtures/projection/` 的正式 wire fixtures。
- provider 新增或修改覆盖流、Tool call、usage、错误映射，并关联 agent/API streaming。
- URL/DNS 变化覆盖私网、localhost、rebinding、redirect 与 pinning。
