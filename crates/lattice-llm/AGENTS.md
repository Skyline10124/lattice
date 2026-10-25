# lattice-llm 工作指引

## 职责

维护请求网关，它是对外唯一的模型请求入口，内部包含协议适配器（请求构造、流式事件解析、用量换算）、provider 注册表、模型元数据、凭据库与 Sign in with ChatGPT、工具 wire 类型、SSE、DNS pinning 与传输层重试判断；另维护日志设置。持久消息与 usage 直接使用 `lattice-conversation` 类型，不负责 Session 图、profile 发现、Agent 循环、重试策略或 LANCET 执行。

- `types.rs`：请求信封、工具定义、流事件与目标；`client.rs`：HTTP 行为、Turn 内 client 复用和连接安全；`transport.rs`：请求标识、错误诊断、可重试判断与 Retry-After。
- `providers/`：各协议的信封、认证、SSE 累积；`provider_projection/` 是唯一生产消息 codec。
- `sse.rs`：流解析；provider 注册表与元数据：内置表、`providers.toml`、模型引用解析与元数据合并；凭据库：`auth.json` 与 SIWC；`stub.rs`：测试客户端；`logging.rs`：日志初始化。
- 共享 URL/DNS/地址检查归 `net-guard`，client 将中立错误映射为 `LlmError`。凭据只经凭据库读写，脱敏所用的已知值也取自凭据库；新增凭据种类时一并纳入脱敏。

## 上位文档

修改前读取[模型接入合同](../../docs/architecture/llm.md)、[会话的对话与模型请求合同](../../docs/architecture/session.md)、[测试工程](../../docs/development/testing.md)；Tool 网络路径由 [LANCET 合同](../../docs/architecture/lancet.md)另行维护，不能覆盖本层 provider 策略。

## 局部约束

- 调用方只给出 provider `id`、模型 `id`、推理档位与请求内容，不接触 endpoint、key 与 features；网关不向外暴露连接材料，每次请求按当前配置与凭据解析。
- 协议差异留在适配器与 client 层，高层只消费类型化 `LlmEvent`、`LlmError` 和模型元数据。适配器按协议划分，厂商差异只用该协议的 features 表达，不按 URL 或 provider `id` 判断厂商；新增 feature 先修订模型接入合同。
- 所有请求 builder 经 `request_messages_to_wire`；完整消息转换集中在 `provider_projection`。OpenAI 流通过 `openai_parts_to_native` 保留到达顺序，不重造固定字段顺序；非流 wire 通过 `wire_to_native`，usage 经 `wire_usage`。不新建 ToolCall/ToolResult codec，组装经 `AttemptAssembly`，超限返回 `ProjectionError::OutputLimitExceeded`，不返回部分消息。
- provider call ID 是可选不透明 wire 元数据，不是领域 Invocation 身份，不合成 `tc_*` 身份。回放来源（协议与 provider `id`）相同时保留真实 ID、签名及 part 顺序；来源不同时去掉 replay state、provider call ID 与 reasoning，不泄露来源 provider 状态。
- `validate_base_url` 的 DNS 检查仅尽力而为，连接前解析并固定地址才是主要 SSRF 防线：每个 Turn 第一次请求时在 blocking 线程上解析、校验并固定地址，构建的 client 在本 Turn 内复用；响应没有完整结束（发送失败、流没交付 `Done` 就结束或出错、错误正文读到一半中断）时丢弃，下次尝试重新解析。不跨 Turn 复用，认证头不放进 client 默认头；SIWC 的 access token 每次请求前取得。localhost、127.0.0.1、::1 使用一致的 loopback 例外，其他私有/保留 IP literal 拒绝。
- 一次尝试对应一个被 provider 接收的请求。HTTP 库只重发 provider 明确没处理的请求：reqwest 那层换成自有策略（判断同默认，每次重发记 WARN），hyper-util 对未发出请求的重发无法关闭，以新建连接的 DEBUG 日志间接可见。不得让 HTTP 库重发已被处理的请求；其他重试一律由协调层按冻结策略执行并逐次记录。
- 除认证头与协议要求的头外，请求头只加 `User-Agent`（LATTICE 自己的标识，不冒充其他客户端）、`Accept` 与取 SessionId 的 `x-client-request-id`；新增发给 provider 的头先修订会话合同。
- HTTP 默认 `redirect::Policy::none()`；重新允许重定向必须逐跳重新执行地址与 pinning 检查，避免向未经验证的主机转送 body 和密钥。不能依赖首跳校验保护后续跳。
- DNS rebinding 防御结合连接地址固定和 `net-guard` 分类/后缀规则。扩大启发式或后缀范围须有测试，并如实报告未覆盖风险。
- 工具名原样作为 wire 名发给每个 provider，不做编解码，命名规则见 [LANCET 合同](../../docs/architecture/lancet.md)。
- 日志写 stderr，ANSI 取决于交互 stderr 与 `NO_COLOR`，不污染 JSONL stdout。输出上限只在协议要求时发送，取值见[模型接入合同](../../docs/architecture/llm.md#适配器的职责)，不在别处硬编码；JSON 解析不以哨兵错误值承担正常控制流。

## 验证入口

- `cargo test -p lattice-llm`；projection 对照 `tests/fixtures/projection/` 的正式 wire fixtures。
- 协议适配器或 feature 新增、修改时覆盖流、Tool call、usage、错误映射与该 feature 每个取值的请求体，并关联 agent/API streaming；其余检查对象见[模型接入合同](../../docs/architecture/llm.md#验证与承接)。
- URL/DNS 变化覆盖私网、localhost、rebinding、redirect 与 pinning。
