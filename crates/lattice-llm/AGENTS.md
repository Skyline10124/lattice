# lattice-llm 工作指引

## 职责

维护请求网关，它是对外唯一的模型请求入口，内部包含协议适配器（请求构造、流式事件解析、用量换算）、provider 注册表、模型元数据、凭据库与 Sign in with ChatGPT、工具 wire 类型、SSE、DNS pinning 与传输层重试判断；另维护日志设置。持久消息与 usage 直接使用 `lattice-conversation` 类型，不负责 Session 图、profile 发现、Agent 循环、重试策略或 LANCET 执行。

目录按职责划分，协议与 provider 分开。解析器与完整消息投影已搬入 `protocol/`；其余目标结构仍需把现有的 `router.rs`、`endpoints.rs` 与 `connection.rs` 拆入对应位置：

- `gateway.rs`：请求网关，对外唯一的请求入口。
- `protocol/`：协议适配器，每个协议一个模块（`chat_completions`、`responses`、`anthropic_messages`、`generate_content`、`interactions`），负责该协议的请求体、流式事件、用量换算与 features；各适配器共用的消息投影 `protocol/projection/` 是唯一的生产消息 codec。这里不出现任何厂商名。
- `provider/`：provider 注册表，包括内置表、`providers.toml` 的解析与合并、按 `id` 查找。这里不含协议代码。
- `metadata/`：内置快照、缓存与手写元数据的合并与刷新。
- `auth/`：凭据库，`auth.json` 与 Sign in with ChatGPT。
- `transport.rs`：请求标识、错误诊断、可重试判断与 Retry-After；`client.rs`：HTTP 行为、Turn 内 client 复用与连接安全；`sse.rs`：流解析；`types.rs`：请求信封、工具定义与流事件。
- `reply.rs`：同步回复累积器，统一有序部件、实时文字投影、限额与完整消息；`reply_stream.rs`：惰性流消费、取消与 deadline、唯一终态交付。
- `stub.rs`：测试客户端；`logging.rs`：日志初始化。
- 共享 URL/DNS/地址检查归 `net-guard`，client 将中立错误映射为 `LlmError`。凭据只经凭据库读写，脱敏所用的已知值也取自凭据库；新增凭据种类时一并纳入脱敏。

## 上位文档

修改前读取[模型接入合同](../../docs/architecture/llm.md)、[会话的对话与模型请求合同](../../docs/architecture/session.md)、[测试工程](../../docs/development/testing.md)；Tool 网络路径由 [LANCET 合同](../../docs/architecture/lancet.md)另行维护，不能覆盖本层 provider 策略。

## 局部约束

- 调用方只给出 provider `id`、模型 `id`、推理档位与请求内容，不接触 endpoint、key 与 features；网关不向外暴露连接材料，每次请求按当前配置与凭据解析。
- 协议差异留在适配器与 client 层，高层只消费类型化 `LlmEvent`、`LlmError` 和模型元数据。适配器按协议划分，厂商差异只用该协议的 features 表达，不按 URL 或 provider `id` 判断厂商；新增 feature 先修订模型接入合同。
- 所有请求 builder 经 `request_messages_to_wire`；`protocol/projection` 的 `wire_to_native` 只用于完整 wire 夹具的解码，逐块交给共同回复累积器，usage 经 `wire_usage`。解析器只翻译部件增量，不保留完整内容缓冲；正式流、stub 与测试客户端共用 `reply_stream`，显示增量与完整消息出自同一次累积。累积器独占流式 `AttemptAssembly`，超限停止消费且不返回部分消息；完整 wire 夹具路径映射为 `ProjectionError::OutputLimitExceeded`。不新建 ToolCall/ToolResult codec。
- provider call ID 是可选不透明 wire 元数据，不是领域 Invocation 身份，不合成 `tc_*` 身份。回放来源（协议与 provider `id`）相同时保留真实 ID、签名及 part 顺序；来源不同时去掉 replay state、provider call ID 与 reasoning，不泄露来源 provider 状态。
- `validate_base_url` 的 DNS 检查仅尽力而为，连接前解析并固定地址才是主要 SSRF 防线：每个 Turn 第一次请求时在 blocking 线程上解析、校验并固定地址，构建的 client 在本 Turn 内复用；响应没有完整结束（发送失败、流没交付 `Done` 就结束或出错、错误正文读到一半中断）时丢弃，下次尝试重新解析。不跨 Turn 复用，认证头不放进 client 默认头；SIWC 的 access token 每次请求前取得。localhost、127.0.0.1、::1 使用一致的 loopback 例外，其他私有/保留 IP literal 拒绝。
- 一次尝试对应一个被 provider 接收的请求。HTTP 库只重发 provider 明确没处理的请求：reqwest 那层换成自有策略（判断同默认，每次重发记 WARN），hyper-util 对未发出请求的重发无法关闭，以新建连接的 DEBUG 日志间接可见。不得让 HTTP 库重发已被处理的请求；其他重试一律由协调层按冻结策略执行并逐次记录。
- 除认证头与协议要求的头外，请求头只加 `User-Agent`（LATTICE 自己的标识，不冒充其他客户端）、`Accept` 与取 SessionId 的 `x-client-request-id`；新增发给 provider 的头先修订会话合同。
- HTTP 默认 `redirect::Policy::none()`；重新允许重定向必须逐跳重新执行地址与 pinning 检查，避免向未经验证的主机转送 body 和密钥。不能依赖首跳校验保护后续跳。
- DNS rebinding 防御结合连接地址固定和 `net-guard` 分类/后缀规则。扩大启发式或后缀范围须有测试，并如实报告未覆盖风险。
- 工具名原样作为 wire 名发给每个 provider，不做编解码，命名规则见 [LANCET 合同](../../docs/architecture/lancet.md)。
- 日志写 stderr，ANSI 取决于交互 stderr 与 `NO_COLOR`，不污染 JSONL stdout。输出上限只在协议要求时发送，取值见[模型接入合同](../../docs/architecture/llm.md#适配器的职责)，不在别处硬编码；JSON 解析不以哨兵错误值承担正常控制流。

## 验证入口

- 运行 `cargo test -p lattice-llm`；消息投影的测试对照 `tests/fixtures/projection/` 中的正式 wire fixtures。
- 新增或修改协议适配器、feature 时，测试要覆盖流、Tool call、用量、错误映射，以及该 feature 每个取值的请求体；再运行 agent 与 API 的流式测试。其余检查对象见[模型接入合同](../../docs/architecture/llm.md#验证与承接)。
- 改了 URL 或 DNS 处理时，测试要覆盖私网地址、localhost、DNS rebinding、重定向与地址固定。
