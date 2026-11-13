---
status: active
---

# 模型接入

> 类型：架构合同
> 上位依据：[项目宪章](../../governance/charter.md)、[宿主平台](../platform.md)
> 决策记录：[ADR 0010](../../adr/0010-protocol-adapters-and-configured-providers.md)

## 目的与范围

用户选一个模型、给出凭据，就能让 Turn 的模型请求发到对应的厂商。本合同规定模型请求怎样发出、发到哪里、带什么凭据，以及推理强度、用量与缓存怎样换算。

以下两个场景贯穿全文：

- 用户用 `lattice auth set deepseek` 录入 DeepSeek 的 key，在 profile 中写 `model = "deepseek/deepseek-v4-pro"`、`effort = "high"`，然后提交任务。后台按内置表找到 DeepSeek 的协议与 endpoint，从 `auth.json` 取 key，把档位换成 DeepSeek 的推理参数，请求结束后把用量换算为统一口径并估算费用。
- 用户用 `lattice auth login chatgpt` 登录 ChatGPT 账号，在 TUI 中用 `/model` 选 `chatgpt/<模型>`。后台用订阅登录的 access token 请求 Responses API，快过期时在请求前刷新。

本合同是协议、provider 配置、features、模型元数据、推理档位、用量换算、凭据保存与订阅登录规则的唯一出处。`configure` 如何加载 profile 写入会话设置、提交如何读取会话设置冻结一轮配置，以及用量如何保存在节点上，由[会话合同](../session/README.md)维护；TUI 的选择面板由 [TUI 合同](../tui/README.md)维护。

## 概念

| 概念 | 含义 |
| --- | --- |
| 协议 | 一种线上请求与流式响应的格式。只有五个：Chat Completions、Responses、Anthropic Messages、Google generateContent、Google Interactions。每个协议有一个适配器 |
| provider | 一个可以接收模型请求的服务，由配置描述：标识、协议、endpoint、认证方式、features 与元数据条目。官方厂商、兼容厂商与本地服务用同一种描述 |
| feature | 某个协议适配器支持的一项可选行为，用来表达厂商对协议的改动，例如推理参数的写法。取值是布尔量或有限枚举，默认值等于标准协议行为 |
| 模型引用 | `<provider>/<model>`，在第一个 `/` 处拆分，例如 `openrouter/anthropic/claude-sonnet-4.6` 的 provider 是 `openrouter` |
| 模型元数据 | 上下文窗口、输出上限、推理档位、工具支持、输入模态与价格，取自 [models.dev](https://models.dev) |
| 推理档位 | `off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`，按此顺序递增 |
| 用量 | 一次尝试的输入、缓存读、缓存写、输出与其中的推理 token 数 |
| 凭据 | API key 或订阅登录材料，按 provider 保存在 `auth.json` |

provider 配置、凭据与模型元数据是用户级的全局状态，由后台读取；它们不随提交它的 shell、项目或前端变化。

## 组成与归属

| 部分 | 负责的工作 |
| --- | --- |
| 请求网关（`lattice-llm`） | 对外唯一的模型请求入口：调用方给出 provider `id`、模型 `id`、推理档位、对话与工具定义、SessionId 及取消与 deadline，得到流式事件、用量与费用估算；并提供无凭据的模型档位能力查询（`model_supported_efforts`）。协议适配、注册表、元数据、凭据与可用性判定都在网关内部 |
| 协议适配器（网关内部） | 按协议与 features 构造请求、解析流式事件、换算用量；把线上事件翻译为部件增量，交给回复组装 |
| provider 注册表（网关内部） | 合并内置表与 `providers.toml`，记录声明事实并校验字段，按 `id` 找到 provider |
| 元数据（网关内部） | 合并内置快照、缓存与 `providers.toml` 中手写的元数据；后台定时刷新缓存 |
| 凭据库（网关内部） | 读写 `auth.json`；SIWC 的登录、刷新与撤销 |
| 可用性与请求准备（网关内部） | 统一判定 provider 与模型的可用性（`ready`、`unconfigured`、`invalid`）；`model_supported_efforts` 只读取 `providers.toml`、模型元数据与 provider features 而不读取凭据；`GatewayTurn::prepare` 单次读取配置与凭据完成提交准入核对 |
| 后台（`lattice-server`） | `configure` 加载 profile 并经无凭据档位能力查询计算会话设置；提交时使用会话设置，经请求网关完成模型与凭据准备；执行中经请求网关发出模型请求；提供凭据、登录、模型候选与元数据刷新的命令和查询 |
| 前端（CLI、TUI） | 发起会话配置与任务提交；凭据录入与登录只负责界面，经私有 IPC 交给后台 |

调用方不接触 endpoint、key 与 features。网关每次请求按当前的内置表、`providers.toml`、元数据与凭据解析，不按 Turn 冻结；一轮固定的只有记在 UserMessage 节点上的 provider `id`、模型 `id` 与推理档位，见[会话合同](../session/acceptance.md#执行配置冻结)。地址固定与连接复用由网关内部按 Turn 维护，规则见[会话合同的模型 HTTP 传输](../session/concurrency.md#输入与传输限制)。

前端不读 provider 配置、元数据或凭据。后台按启动时的环境确定配置目录与 TLS 根证书，此后不随提交变化，见[会话合同的配置来源](../session/acceptance.md#配置来源与后台启动)。

## 文件索引

| 文件 | 内容 |
| --- | --- |
| [protocols.md](protocols.md) | 协议适配器（端点、认证、职责、回放来源）与各协议 features |
| [providers.md](providers.md) | provider 配置字段、内置 provider 表、`providers.toml` 与 endpoint 来源 |
| [models.md](models.md) | 模型引用、元数据来源与合并、可用性判定及模型候选查询 |
| [reasoning.md](reasoning.md) | 可用推理档位的推导、token 预算与各协议档位映射 |
| [usage.md](usage.md) | 用量换算、缓存的开启与检测，以及费用估算与价格表比对 |
| [credentials.md](credentials.md) | `auth.json`、凭据命令与 Sign in with ChatGPT 登录、刷新、登出及账号模型列表 |

## 限制

| 对象 | 限制 | 依据 |
| --- | --- | --- |
| `providers.toml` | 最多 1 MiB；文件中的 provider 表最多 256 个（覆盖内置 provider 的表也算），每个 provider 手写的模型最多 1,024 个 | 实现常量，防止异常文件占用内存 |
| `auth.json` | 最多 1 MiB；key 1～16,384 字节 | 沿用现有连接材料的 key 上限 |
| models.dev 拉取 | 响应最多 32 MiB，30 秒内读完 | 2026-10-24 的 `api.json` 约 5.9 MB |
| ChatGPT 账号目录 | GET 响应最多 32 MiB，连接准备至读完不超过 30 秒；本地缓存读取和发布最多 32 MiB | 复用元数据拉取边界；凭据刷新沿用自身时限 |
| SIWC 刷新余量 | 剩余有效期短于 300 秒时刷新 | access token 有效期 1 小时；余量覆盖发请求的时间与时钟偏差 |

Turn deadline 的范围由[会话合同](../session/concurrency.md#输入与传输限制)维护。

## 验证与承接

| 检查对象 | 入口 |
| --- | --- |
| 各协议的请求体与流式事件：文字、推理、多个工具调用、用量、错误事件 | `cargo test -p lattice-llm`，夹具取自官方文档与脱敏的真实响应 |
| 每个 feature 取值的请求体，默认值与标准协议形状一致 | 同上 |
| provider 注册表：内置表与 `providers.toml` 的合并、严格解析、`chatgpt` 不能改写、非 loopback 必须 HTTPS | 同上 |
| 元数据：来源的选择与按字段覆盖、只保留列出的字段、缓存保留 `providers.toml` 中的 `catalog` 条目、刷新失败继续使用上一份 | 同上 |
| 档位：可用档位的推导、按 token 数的预算、各协议的映射、不支持的档位被拒绝 | 同上 |
| 用量换算与费用估算：与价格表比对得出 complete/partial 与确切/估算/未知，缺项不借用 input 价，显式 0 算已报告，generateContent 完成时补零、Messages 输入侧 delta-0、Chat 缓存写别名，以及格式错误与换算溢出 | 同上 |
| `auth.json`：权限检查、原子写入、严格解析、凭据不进入日志与公开输出 | 同上 |
| endpoint 与凭据来源的负向测试：项目目录中的 `providers.toml` 不被读取，环境变量与工作区 `.env` 既不能改变 endpoint、也不能提供 key | harness 的 fast 场景 |
| SIWC 的本地逻辑：用注入的时钟与构造的短有效期 token 记录，核对剩余有效期短于余量（包括已经过期）时先刷新、有效期内不刷新，并发的请求只刷新一次，刷新失败按上文三类处理并写回 `auth.json`；回调的 `state` 与 `client_id` 校验，登录中途取消时关闭监听。不模拟 OpenAI 的授权服务 | `lattice-llm` 局部测试 |
| 真实 provider | 有凭据时验证 OpenAI（Chat Completions、Responses、推理、缓存）、ChatGPT 订阅（SIWC）、Gemini（generateContent、Interactions）；SIWC 与 OpenAI 的交互（登录、id token 校验、刷新、登出与账号模型列表）只由真实账号验证，CI 不覆盖；按 access token 的到期时间选时机，验证 Turn 中途的刷新，以及流跨过过期时刻会不会被断开。没有凭据的 provider 只按文档与夹具验证，交付时说明 |

内置表与 features 的依据：OpenAI [推理](https://developers.openai.com/api/docs/guides/reasoning)与[缓存](https://developers.openai.com/api/docs/guides/prompt-caching)，OpenAI SIWC [登录](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)、[账户与会话](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions)、[模型与推理](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)、[错误与恢复](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery)，Google [Interactions API](https://ai.google.dev/api/interactions-api) 与 [generateContent](https://ai.google.dev/api/generate-content)，xAI [推理](https://docs.x.ai/docs/guides/reasoning)，Mistral [推理](https://docs.mistral.ai/capabilities/reasoning/)，DeepSeek [思考模式](https://api-docs.deepseek.com/guides/thinking_mode)与[上下文缓存](https://api-docs.deepseek.com/guides/kv_cache)，Kimi [思考模型](https://platform.kimi.ai/docs/guide/use-kimi-k2-thinking-model)，Z.ai [思考模式](https://docs.z.ai/guides/capabilities/thinking-mode)，智谱 [思考模式](https://docs.bigmodel.cn/cn/guide/capabilities/thinking-mode)，阿里云 [深度思考](https://www.alibabacloud.com/help/en/model-studio/deep-thinking)、[上下文缓存](https://www.alibabacloud.com/help/en/model-studio/context-cache)与 [OpenAI 兼容](https://www.alibabacloud.com/help/en/model-studio/compatibility-of-openai-with-dashscope)，MiniMax [国际站](https://platform.minimax.io/docs/api-reference/text-anthropic-api)与[国内站](https://platform.minimax.cn/docs/api-reference/text-anthropic-api)，硅基流动 [推理](https://docs.siliconflow.cn/cn/userguide/capabilities/reasoning)，OpenRouter [推理](https://openrouter.ai/docs/use-cases/reasoning-tokens)，Groq [推理](https://console.groq.com/docs/reasoning)，均于 2026-10-25 查阅。火山方舟的推理参数没有取得官方文档，`volcengine` 的 features 与 Anthropic 兼容厂商的缓存支持在实现时核对，结论写回本表。

决策理由见 [ADR 0010](../../adr/0010-protocol-adapters-and-configured-providers.md)；局部细则由 [`lattice-llm`](../../../crates/lattice-llm/AGENTS.md) 入口承接。
