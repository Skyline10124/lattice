# lattice-llm 工作指引

维护对外唯一的模型请求网关、五种协议适配器、provider 注册表、模型元数据、凭据与 Sign in with ChatGPT 及 stderr 日志初始化，被 `lattice-agent`、`lattice-api` 与 `xtask` 调用。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 重导出请求网关、协议、模型候选、认证、流事件与日志接口 |
| `src/gateway.rs`、`src/gateway/` | `RequestGateway`（筛选并缓存可用 TLS 根）、`GatewayTurn::prepare` 准入与档位选择、`GatewayTurn::resolve` 按次解析及 `cost.rs` 费用估算 |
| `src/availability.rs` | 统一的 provider 与模型 `Availability`（`Ready` / `Unconfigured` / `Invalid`）判定与补救提示 |
| `src/protocol/` | 五种协议适配器（`chat_completions.rs`、`responses.rs`、`anthropic_messages.rs`、`generate_content.rs`、`interactions.rs`）、`features.rs`、`usage.rs`、`common.rs` 及唯一生产消息投影 `projection/`（含工具结果与 `ToolError` 载荷编码） |
| `src/provider/` | 内置 provider 表（`builtins.rs`）、`providers.toml` 严格解析与合并（`config.rs`，含显式声明 `declared` 事实）及按 `ProviderId` 查找 |
| `src/metadata/`、`src/models.rs`、`src/reasoning.rs` | 内置快照、`models.dev` 与 ChatGPT 账号目录单次拉取与缓存合并、`ModelCandidateQuery` 候选查询及可用推理档位推导与映射 |
| `src/auth/` | `auth.json` 读写、脱敏已知凭据值提取，以及 `siwc/` 下的 Sign in with ChatGPT 登录、单飞刷新、登出与分阶段收尾（`stop_logins` / `shutdown`） |
| `src/tls.rs`、`src/client.rs`、`src/transport.rs`、`src/sse.rs`、`src/types.rs` | 平台根 DER 原始加载、单 Turn 地址固定与 HTTP 客户端复用、请求头与未处理请求重发策略、错误诊断与 Retry-After 解析、SSE 解析及请求/事件类型 |
| `src/reply.rs`、`src/reply_stream.rs` | 同步回复累积器（有序片段、实时增量、字节限额）与惰性流包装器（取消、deadline、唯一终态交付） |
| `src/logging.rs`、`src/stub.rs`、`src/testing.rs` | stderr 日志与 `NO_COLOR` 控制、`StubLlm` 及 `test-utils` 流测试辅助 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [模型接入](../../docs/architecture/llm/README.md)、[工程规范](../../docs/development/engineering.md) |
| 协议适配器、消息投影与 features | [协议适配器与 features](../../docs/architecture/llm/protocols.md)、[对话数据与模型投影](../../docs/architecture/session/conversation.md)、[Step 执行与结果保存](../../docs/architecture/session/steps.md#调用槽位) |
| provider 注册表、URL 与 HTTP 传输 | [provider 配置](../../docs/architecture/llm/providers.md)、[会话存储与并发](../../docs/architecture/session/concurrency.md#输入与传输限制)、[Turn 的接受与模型步骤](../../docs/architecture/session/acceptance.md#模型请求与执行预算) |
| 模型元数据、可用性、推理档位与计费 | [模型引用与元数据](../../docs/architecture/llm/models.md)、[推理档位](../../docs/architecture/llm/reasoning.md)、[用量、缓存与费用](../../docs/architecture/llm/usage.md) |
| `auth.json` 与 Sign in with ChatGPT | [凭据与 Sign in with ChatGPT](../../docs/architecture/llm/credentials.md)、[重开、取消与未完成状态](../../docs/architecture/session/recovery.md#停止后台) |
| 工具 wire 名称与验证分层 | [ADR 0009](../../docs/adr/0009-remove-legacy-lancet-tool-executor.md)、[测试工程](../../docs/development/testing.md) |

## 约束

- `RequestGateway` 对调用方封装 endpoint、凭据与协议 `features`，调用方只传入 `ProviderId`、`ModelId`、推理档位与类型化请求内容；网关在 `new` 时筛选并缓存平台 TLS 根，每次请求经 `GatewayTurn::resolve` 读取当前配置与凭据，见[组成与归属](../../docs/architecture/llm/README.md#组成与归属)与[配置来源与后台启动](../../docs/architecture/session/acceptance.md#配置来源与后台启动)。
- 适配器严格按 wire 协议划分，厂商差异全部通过该协议的 `features` 表达（模块内保持无厂商名硬编码，新增 feature 先修订[协议适配器与 features](../../docs/architecture/llm/protocols.md#features)）；输出 token 上限仅在协议要求必填时按[适配器的职责](../../docs/architecture/llm/protocols.md#适配器的职责)发送，工具名按原样作为 wire 名称发送。
- 所有请求构造统一经过 `request_messages_with_replay`，回放材料与可选不透明 provider call ID 按[回放来源](../../docs/architecture/llm/protocols.md#回放来源)透传或裁剪，工具失败载荷 `ResultPayload::Failed(ToolError)` 编码为 `{"error":{"code":<code 字符串>,"message":<message>}}`（`message` 按 UTF-8 字符边界限制在 4,096 字节内）；流解析器只翻译部件增量并交给独占 `AttemptAssembly` 的 `reply.rs` 与 `reply_stream.rs`（正式流、`stub` 与测试客户端共用），使实时增量与完整回复同源并在超限时立即停止消费；`wire_to_native` 专用于完整 wire 夹具逐块喂入同一累积器（超限映射为 `ProjectionError::OutputLimitExceeded`），用量统一经 `wire_usage` 换算，JSON 解析以显式条件分支处理正常控制流。
- 凭据统一经 `auth/` 读写，脱敏所需的已知凭据值从同一凭据库提取，新增凭据类型同步纳入提取列表，见 [`auth.json`](../../docs/architecture/llm/credentials.md#authjson)。
- `client.rs` 在每个 Turn 首次请求时于 blocking 线程调用 `net-guard` 解析、校验并固定地址（`localhost`、`127.0.0.1`、`::1` 适用一致的 loopback 例外），在本 Turn 内复用已完整读毕的客户端，响应未完整结束时丢弃客户端以便下次重新解析；认证头与按需获取的 SIWC access token 按次附加，公共请求头固定为自有 `User-Agent`、`Accept` 与 `x-client-request-id`（修订请求头先更新[输入与传输限制](../../docs/architecture/session/concurrency.md#输入与传输限制)）。
- HTTP 客户端固定使用 `redirect::Policy::none()` 关闭自动重定向（若未来开启重定向，须对每一跳重新执行地址解析、校验与固定）；`reqwest` 配置自有重试策略，只重发服务端明确未处理的请求并在每次重发时记 `WARN` 日志，`hyper-util` 对尚未发出请求的重连通过新建连接 `DEBUG` 日志留痕，其余已被接收的请求结果直接交由协调层按冻结预算处理，见[模型请求与执行预算](../../docs/architecture/session/acceptance.md#模型请求与执行预算)。
- `logging.rs` 将日志输出到 stderr，依据交互终端状态与 `NO_COLOR` 控制 ANSI 颜色，保持 stdout 专供结构化输出。
- 本 crate 之外：底层 URL/IP/DNS 分类原语由 `net-guard` 负责；持久对话类型由 `lattice-conversation` 负责；元数据刷新定时与并发调度、模型循环与重试预算由 `lattice-api` 负责；会话树与 profile 发现分别由 `lattice-session` 和 `lattice-profile` 负责。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-llm`（含对照 `tests/fixtures/projection/` 正式 wire 夹具的消息投影测试） |
| 新增或修改协议适配器、`features` | 在 `cargo test -p lattice-llm` 中覆盖流解析、ToolCall、用量、错误映射及该 feature 各取值的请求体，再运行 `cargo test -p lattice-agent` 与 `cargo test -p lattice-api` |
| 修改 URL、DNS、重定向或地址固定 | 在 `cargo test -p lattice-llm` 中覆盖私网与保留地址、`localhost`、DNS rebinding 域名、重定向拒绝与连接地址固定 |
