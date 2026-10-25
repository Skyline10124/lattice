---
status: active
---

# 模型接入

> 类型：架构合同
> 上位依据：[项目宪章](../governance/charter.md)、[宿主平台](platform.md)
> 决策记录：[ADR 0010](../adr/0010-protocol-adapters-and-configured-providers.md)
> 修订记录：2026-10-25 由维护者批准，随 PR #317 合并生效。

## 目的与范围

用户选一个模型、给出凭据，就能让 Turn 的模型请求发到对应的厂商。本合同规定模型请求怎样发出、发到哪里、带什么凭据，以及推理强度、用量与缓存怎样换算。

以下两个场景贯穿全文：

- 用户用 `lattice auth set deepseek` 录入 DeepSeek 的 key，在 profile 中写 `model = "deepseek/deepseek-v4-pro"`、`effort = "high"`，然后提交任务。后台按内置表找到 DeepSeek 的协议与 endpoint，从 `auth.json` 取 key，把档位换成 DeepSeek 的推理参数，请求结束后把用量换算为统一口径并估算费用。
- 用户用 `lattice auth login chatgpt` 登录 ChatGPT 账号，在 TUI 中用 `/model` 选 `chatgpt/<模型>`。后台用订阅登录的 access token 请求 Responses API，快过期时在请求前刷新。

本合同是协议、provider 配置、features、模型元数据、推理档位、用量换算、凭据保存与订阅登录规则的唯一出处。会话如何持有配置（profile 副本）、一轮冻结哪些配置，以及用量如何保存在节点上，由[会话合同](session.md)维护；TUI 的选择面板由 [TUI 合同](tui.md)维护。

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
| 请求网关（`lattice-llm`） | 对外唯一的模型请求入口：调用方给出 provider `id`、模型 `id`、推理档位、对话与工具定义、SessionId 及取消与 deadline，得到流式事件、用量与费用估算。下面四项都在网关内部 |
| 协议适配器（网关内部） | 按协议与 features 构造请求、解析流式事件、换算用量；把线上事件翻译为部件增量，交给回复组装 |
| provider 注册表（网关内部） | 合并内置表与 `providers.toml`，校验字段，按 `id` 找到 provider |
| 元数据（网关内部） | 合并内置快照、缓存与 `providers.toml` 中手写的元数据；后台定时刷新缓存 |
| 凭据库（网关内部） | 读写 `auth.json`；SIWC 的登录、刷新与撤销 |
| 后台（`lattice-api`） | 接受 Turn 时读取 profile，核对 provider、模型、档位与凭据可用；执行中经请求网关发出模型请求；提供凭据、登录、模型候选与元数据刷新的命令和查询 |
| 前端（CLI、TUI） | 交出用户的选择；凭据录入与登录只负责界面，经私有 IPC 交给后台 |

调用方不接触 endpoint、key 与 features。网关每次请求按当前的内置表、`providers.toml`、元数据与凭据解析，不按 Turn 冻结；一轮固定的只有记在 UserMessage 节点上的 provider `id`、模型 `id` 与推理档位，见[会话合同](session.md#执行配置冻结)。地址固定与连接复用由网关内部按 Turn 维护，规则见[会话合同的模型 HTTP 传输](session.md#输入与传输限制)。

前端不读 provider 配置、元数据或凭据。后台按启动时的环境确定配置目录与 TLS 根证书，此后不随提交变化，见[会话合同的配置来源](session.md#配置来源与后台启动)。

## 协议适配器

### 端点与认证

| 协议 | `protocol` 取值 | 请求 | 默认认证 |
| --- | --- | --- | --- |
| Chat Completions | `chat-completions` | `POST {base_url}/chat/completions`，`stream: true` | `bearer` |
| Responses | `responses` | `POST {base_url}/responses`，`stream: true` | `bearer` |
| Anthropic Messages | `anthropic-messages` | `POST {base_url}/v1/messages`，`stream: true`，带 `anthropic-version` | `x-api-key` |
| Google generateContent | `generate-content` | `POST {base_url}/models/{model}:streamGenerateContent?alt=sse` | `x-goog-api-key` |
| Google Interactions | `interactions` | `POST {base_url}/interactions`，`stream: true` | `x-goog-api-key` |

认证方式：

| `auth` | 请求头 |
| --- | --- |
| `bearer` | `Authorization: Bearer <key>` |
| `x-api-key` | `x-api-key: <key>` |
| `x-goog-api-key` | `x-goog-api-key: <key>` |
| `oauth:chatgpt` | `Authorization: Bearer <access token>`，见 [Sign in with ChatGPT](#sign-in-with-chatgpt) |
| `none` | 不带认证头，用于本地服务 |

请求头只有认证头、协议要求的头（`anthropic-version`）与[会话合同](session.md#输入与传输限制)列出的通用头。

### 适配器的职责

- **请求。** 把当前路径上的对话、系统提示、工具定义和推理档位按协议与 features 写成请求体。投影规则与回放材料的处理由[会话合同的回放与跨 provider 投影](session.md#回放与跨-provider-投影)维护。
- **流式事件。** 把线上事件翻译为正文、reasoning 与工具调用的部件增量，交给 #312 的累积器组装完整回复。解析端对无歧义的厂商差异直接兼容，例如 `reasoning_content` 与 `reasoning` 两种字段都读取，Mistral 的 `content` 为分块数组时读取其中的 `thinking` 块。
- **用量。** 按[用量换算](#用量换算)得出统一口径。
- **不使用服务端会话状态。** Responses 与 Interactions 固定发送 `store: false`，每次发送完整历史，不发送 `previous_response_id`、`previous_interaction_id`，不使用后台执行。

适配器不发送采样参数（`temperature`、`top_p` 等），也不发送输出上限（例如 Responses 的 `max_output_tokens`），由服务端按模型的默认值处理；只有 Anthropic Messages 的 `max_tokens` 是必填字段，取模型元数据的输出上限，没有元数据时为 32,768。

### 回放来源

签名、加密 reasoning 等不透明回放材料只对产生它的协议与 provider 有效。每次尝试记录来源 `(protocol, provider id)`；来源相同才回放这些材料，来源不同时按跨来源规则投影。Responses 请求带 `include: ["reasoning.encrypted_content"]`，使 `store: false` 时也能取得可回放的加密 reasoning。

## provider 配置

### 字段

| 字段 | 含义 | 规则 |
| --- | --- | --- |
| `id` | provider 标识，用于模型引用、节点记录、回放来源与凭据查找 | 1～64 个字符，`[a-z0-9]` 开头，只含 `[a-z0-9-]` |
| `protocol` | 上表五个取值之一 | 必填 |
| `base_url` | endpoint | 必填；不含用户名密码、query 与 fragment；非 loopback 地址必须是 `https`；最长 8,192 字节 |
| `auth` | 认证方式 | 默认随协议；`oauth:chatgpt` 只用于内置的 `chatgpt` |
| `features` | 见 [features](#features) | 只能取该协议已定义的 feature 与取值 |
| `catalog` | 对应的 models.dev provider 条目 | 可选；只用于取元数据 |
| `models` | 按模型手写的元数据与 features 覆盖 | 可选 |

### 内置 provider 表

内置表手工维护，编译进程序。每一行的协议、endpoint、认证方式与 features 对照厂商文档核对，变化在 PR 中审查。表中不记录某一家是否经过实测，实际验证范围写在实现 PR 的验证记录中。

| `id` | `protocol` | `base_url` | `auth` | `catalog` | features |
| --- | --- | --- | --- | --- | --- |
| `openai` | `responses` | `https://api.openai.com/v1` | `bearer` | `openai` | — |
| `chatgpt` | `responses` | `https://api.openai.com/v1` | `oauth:chatgpt` | `openai` | — |
| `anthropic` | `anthropic-messages` | `https://api.anthropic.com` | `x-api-key` | `anthropic` | — |
| `google` | `interactions` | `https://generativelanguage.googleapis.com/v1beta` | `x-goog-api-key` | `google` | — |
| `xai` | `responses` | `https://api.x.ai/v1` | `bearer` | `xai` | — |
| `mistral` | `chat-completions` | `https://api.mistral.ai/v1` | `bearer` | `mistral` | `reasoning_replay = "content-chunks"` |
| `deepseek` | `chat-completions` | `https://api.deepseek.com` | `bearer` | `deepseek` | `thinking_toggle = "thinking-type"`、`reasoning_replay = "reasoning-content"` |
| `moonshotai` | `chat-completions` | `https://api.moonshot.ai/v1` | `bearer` | `moonshotai` | 同 `deepseek` |
| `moonshotai-cn` | `chat-completions` | `https://api.moonshot.cn/v1` | `bearer` | `moonshotai-cn` | 同 `deepseek` |
| `zai` | `chat-completions` | `https://api.z.ai/api/paas/v4` | `bearer` | `zai` | 同 `deepseek` |
| `zhipuai` | `chat-completions` | `https://open.bigmodel.cn/api/paas/v4` | `bearer` | `zhipuai` | 同 `deepseek` |
| `alibaba` | `chat-completions` | `https://dashscope-intl.aliyuncs.com/compatible-mode/v1` | `bearer` | `alibaba` | `thinking_toggle = "enable-thinking"`、`thinking_budget = "thinking-budget"`、`cache_markers = true` |
| `alibaba-cn` | `chat-completions` | `https://dashscope.aliyuncs.com/compatible-mode/v1` | `bearer` | `alibaba-cn` | 同 `alibaba` |
| `minimax` | `anthropic-messages` | `https://api.minimax.io/anthropic` | `x-api-key` | `minimax` | `cache = "none"` |
| `minimax-cn` | `anthropic-messages` | `https://api.minimax.cn/anthropic` | `x-api-key` | `minimax-cn` | `cache = "none"` |
| `volcengine` | `chat-completions` | `https://ark.cn-beijing.volces.com/api/v3` | `bearer` | `volcengine` | `thinking_toggle = "thinking-type"` |
| `siliconflow-cn` | `chat-completions` | `https://api.siliconflow.cn/v1` | `bearer` | `siliconflow-cn` | `thinking_toggle = "enable-thinking"`、`thinking_budget = "thinking-budget"` |
| `openrouter` | `chat-completions` | `https://openrouter.ai/api/v1` | `bearer` | `openrouter` | `effort_param = "reasoning-object"`、`thinking_budget = "reasoning-max-tokens"`、`reasoning_replay = "reasoning-content"` |
| `groq` | `chat-completions` | `https://api.groq.com/openai/v1` | `bearer` | `groq` | — |
| `ollama` | `chat-completions` | `http://127.0.0.1:11434/v1` | `none` | — | — |
| `lmstudio` | `chat-completions` | `http://127.0.0.1:1234/v1` | `none` | `lmstudio` | — |

- `google` 的 `generate-content` 保留，可在 `providers.toml` 中切换。OpenAI 的新模型在 Chat Completions 上不能使用工具（[OpenAI 推理指南](https://developers.openai.com/api/docs/guides/reasoning)），所以 `openai` 以 Responses 为准。
- `chatgpt` 是 OpenAI 的订阅通道，不计入第一轮收录的 20 家，限制见 [Sign in with ChatGPT](#sign-in-with-chatgpt)。
- 阿里云的文档推荐按工作空间区分的域名（`https://{WorkspaceId}.<地域>.maas.aliyuncs.com/compatible-mode/v1`），表中用的是仍可使用的旧域名；使用工作空间域名的用户在 `providers.toml` 中覆盖 `base_url`。
- `ollama` 在 models.dev 中没有条目，它的模型没有元数据，可以在 `providers.toml` 中手写。

### 用户配置 `providers.toml`

用户全局配置位于配置目录下的 `providers.toml`（默认 `~/.lattice/providers.toml`）。一个 provider 一张表：表名是内置 `id` 时覆盖该 provider 的字段，否则新增一个 provider。

```toml
[providers.my-vllm]                    # 新增自建服务
protocol = "chat-completions"
base_url = "https://vllm.example.com/v1"

[providers.my-vllm.models."qwen3-32b"] # 元数据中没有的模型，手写元数据
context = 131072
output = 8192
effort = ["off", "high"]

[providers.deepseek]                   # 覆盖内置 provider 的 endpoint
base_url = "https://gateway.example.com/deepseek"

[providers.deepseek.features]
thinking_toggle = "thinking-type"

[providers.deepseek.models."deepseek-v4-flash".features]
reasoning_replay = "none"              # 按模型覆盖 feature
```

| 字段 | 位置 | 说明 |
| --- | --- | --- |
| `protocol`、`base_url`、`auth`、`catalog` | provider 表 | 见[字段](#字段)；覆盖内置 provider 时只写要改的字段 |
| `features` | provider 子表 | 覆盖或补充 provider 级 features |
| `models."<模型>"` | provider 子表 | 手写元数据：`context`、`output`（token 数），`effort`（可用档位列表），`tools`（布尔，默认 true），`cost`（`input`、`output`、`cache_read`、`cache_write`，美元每百万 token） |
| `models."<模型>".features` | 模型子表 | 按模型覆盖 features |

- **严格解析。** 未知字段、未知 feature 或取值、类型不符、格式错误都使整个文件无效。文件无效时，用到 provider 配置的操作在接受前按配置错误拒绝，诊断写明文件路径、行号与原因；不使用部分有效的内容。
- **文件不存在**等于空配置，只使用内置表。
- **`chatgpt` 不能改写。** 文件中出现 `[providers.chatgpt]` 的 `protocol`、`base_url`、`auth` 或 `features`，或其他 provider 设置 `auth = "oauth:chatgpt"`，整个文件无效。`chatgpt` 只能写 `models` 下的手写元数据。
- **不写凭据。** 文件中没有 key 字段；凭据只在 `auth.json` 中。
- **生效时机。** 请求网关每次请求、后台接受 Turn 与列出模型候选时，都按文件的当前内容解析，修改从下一次请求起对所有会话生效；执行中的一轮可能前后两次请求发往不同的 endpoint。文件在执行中变为无效时，下一次请求按不可重试的配置错误停止。

### endpoint 的来源

key 会发到 endpoint，所以 endpoint 只来自内置表与用户全局的 `providers.toml`：

- 项目目录中的任何文件都不提供 provider 配置。agent profile 可以放在项目内，它只能写模型引用，不能改变 endpoint 或认证方式。
- 环境变量与工作区 `.env` 不提供 key、endpoint 或模型。原有的 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`GEMINI_API_KEY`、`OPENAI_COMPATIBLE_BASE_URL`、`OPENAI_COMPATIBLE_MODEL`、`OPENAI_COMPATIBLE_API_KEY`、`ANTHROPIC_BASE_URL`、`GEMINI_BASE_URL` 不再读取。
- 模型元数据只有[元数据的来源与合并](#元数据的来源与合并)中列出的字段，不含 endpoint。

地址检查与连接时的地址固定沿用[会话合同的模型 HTTP 传输](session.md#输入与传输限制)与 `net-guard` 的规则：localhost、127.0.0.1、::1 是 loopback，其他私有与保留地址拒绝。

## features

feature 由协议适配器的代码定义，配置只能在已有取值中选择。下表是第一轮的全部 feature；新的厂商差异先在适配器中增加取值并测试，再在内置表或 `providers.toml` 中启用。

### Chat Completions

| feature | 取值 | 默认 | 作用 |
| --- | --- | --- | --- |
| `effort_param` | `reasoning-effort`、`reasoning-object` | `reasoning-effort` | 推理档位写作顶层 `reasoning_effort: "<档位>"`，或 OpenRouter 的 `reasoning: {"effort": "<档位>"}`；`off` 写作 `"none"` |
| `thinking_toggle` | `none`、`thinking-type`、`enable-thinking` | `none` | 关闭与开启推理的写法：`none` 时来自 toggle 项的 `off` 与 `high` 不可用；`thinking-type` 写 `thinking: {"type": "enabled" \| "disabled"}`；`enable-thinking` 写 `enable_thinking: true \| false` |
| `thinking_budget` | `none`、`thinking-budget`、`reasoning-max-tokens` | `none` | 按 token 数限制推理的写法：顶层 `thinking_budget: N`，或 OpenRouter 的 `reasoning: {"max_tokens": N}` |
| `reasoning_replay` | `none`、`reasoning-content`、`content-chunks` | `none` | 同来源回放时，助手消息中的 reasoning 怎样发回：不发；写作 `reasoning_content` 字段；写作 Mistral 的 `content` 分块（`thinking` 块在前） |
| `cache_markers` | 布尔 | `false` | 在系统提示与最后一条消息的末尾内容块上加 `cache_control: {"type": "ephemeral"}`，用于阿里云的显式缓存 |

推理参数各部分的组合见[档位映射](#档位映射)。

### Responses

第一轮没有 feature。适配器只发送 SIWC 也接受的字段（见[适配器的职责](#适配器的职责)），所以 `chatgpt` 不需要额外的 feature。

### Anthropic Messages

| feature | 取值 | 默认 | 作用 |
| --- | --- | --- | --- |
| `cache` | `automatic`、`none` | `automatic` | `automatic`：系统提示最后一块加显式断点，请求顶层加 `cache_control: {"type": "ephemeral"}` 由服务端放置对话末尾的断点；`none`：不发送缓存字段 |

### Google generateContent 与 Interactions

第一轮没有 feature。

### 现有厂商差异的迁移

| 现有代码中的行为 | 迁移后 |
| --- | --- |
| `ProviderKind` 决定认证头与请求路径（`crates/lattice-llm/src/client.rs`） | 由 `protocol` 与 `auth` 决定 |
| Chat Completions 总是在助手消息中发回 `reasoning_content`（`provider_projection/openai.rs`） | 由 `reasoning_replay` 决定，默认不发；内置表中需要它的厂商启用 `reasoning-content` |
| Chat Completions 同时读取 `reasoning_content` 与 `reasoning` | 保持，属于解析端的兼容 |
| Chat Completions 总是发送 `stream_options: {"include_usage": true}` | 保持，属于标准协议行为 |
| Anthropic `max_tokens` 固定为 32,768 | 取模型元数据的输出上限，没有元数据时为 32,768 |
| generateContent 的工具 schema 用 `parametersJsonSchema`（#270） | 保持，属于该协议的写法 |
| OpenAI 兼容路径的工具名映射 | 已由 #311 删除，工具名原样发送 |

## 模型引用与元数据

### 模型引用

- profile 的 `[agent].model`、CLI 的 `--model` 与 TUI 的选择都写作 `<provider>/<model>`。在第一个 `/` 处拆分；provider 部分按 `id` 规则校验，模型部分非空、最长 4,096 字节、不含控制字符。
- 没有别名，也没有默认模型。profile 没有写模型、会话也没有选择时，提交在接受前按配置错误拒绝。
- provider 不存在时按配置错误拒绝，说明列出可用的 provider。模型不在元数据中时照常使用，只是没有价格与推理档位。

### 元数据的来源与合并

| 来源 | 内容 | 更新 |
| --- | --- | --- |
| 内置快照 | xtask 从 models.dev 生成，只含内置表 `catalog` 所列的条目 | 随版本发布，变化在 PR 中审查 |
| 缓存 `cache/models.json` | 后台从 `https://models.dev/api.json` 拉取并按快照的规则过滤 | 后台启动时与此后每 24 小时；`lattice models refresh` 立即刷新 |
| `providers.toml` 的 `models` | 用户手写 | 用户编辑 |

- **选择来源。** 缓存存在且能解析时整份使用缓存，否则使用内置快照；缓存中缺少某个 `catalog` 条目时，这个条目使用快照。手写的元数据按字段覆盖上述结果，只写在 `providers.toml` 中的模型由手写值单独构成。
- **保留的字段。** 模型 `id`、`name`、`limit.context`、`limit.output`、`reasoning`、`reasoning_options`、`tool_call`、`modalities.input`、`cost.input`、`cost.output`、`cost.cache_read`、`cost.cache_write`、`cost.reasoning` 与 `status`。其余字段丢弃。
- **没有过期的概念。** 缓存旧了只影响列表与价格的准确性，不影响请求发往哪里，不提示过期。
- **刷新失败**只记日志，继续使用上一份数据；`lattice models refresh` 把失败原因返回给用户。拉取使用 HTTPS 与固定地址的连接，响应最多 32 MiB，30 秒内没有读完即放弃。新缓存先写临时文件再改名替换。
- **请求路径上不联网。** 接受 Turn 与每次请求时只读上述数据。

### 模型候选

模型候选查询列出可用的 `<provider>/<model>`，供 TUI 的 `/model` 与 CLI 的 `lattice models` 使用：

- provider 来自内置表与 `providers.toml`，模型来自上述合并结果；`chatgpt` 只列出[账号模型列表](#账号模型列表)中的模型。
- `status` 为 `deprecated` 的模型不列出，仍可按名字使用。
- 每个候选带上下文窗口、输出上限、可用的推理档位、价格，以及凭据是否就绪（`auth = "none"` 的 provider 总是就绪）。

## 推理档位

### 可用档位

模型可用的档位由元数据的 `reasoning_options` 得出，手写元数据的 `effort` 列表直接给出可用档位：

| `reasoning_options` 中的项 | 增加的档位 |
| --- | --- |
| `{"type": "effort", "values": [...]}` | 列出的档位；`none` 记作 `off`，`default` 忽略 |
| `{"type": "toggle"}` | `off`；同一模型没有 `effort` 或 `budget_tokens` 项时，另加 `high`，表示只写开启、不写深度，由服务端按默认深度推理 |
| `{"type": "budget_tokens", "min": …, "max": …}`，且同一模型没有 `effort` 项 | `low`、`medium`、`high`、`max`，按下文的预算去掉重复的档位 |

- 每个档位只用产生它的那一项对应的字段表达，不跨类型回退：来自 toggle 项的 `off` 与 `high` 只写开关字段，来自 effort 项的档位（包括取值 `none` 得到的 `off`）只写档位字段，来自 budget_tokens 项的档位只写预算字段。所选协议与 features 写不出某一档时，它不出现在可用档位中。
- `reasoning` 为 true 而没有 `reasoning_options` 的模型总在推理，没有可选档位。
- 选了模型不支持的档位时，在接受前按配置错误拒绝，不降级到相近档位。
- profile 的 `[agent].effort` 设默认档位；没有档位时不发送任何推理参数，由 provider 使用默认行为。

按 token 数限制的档位取以下预算：

| 档位 | 预算 |
| --- | --- |
| `low` | 2,048 |
| `medium` | 8,192 |
| `high` | 16,384 |
| `max` | 32,768 |

- 预算先夹到模型的 `[min, max]` 之内（缺的一端不限），再限制为不超过输出上限的一半，给正文留出空间；没有输出上限时按 32,768 计，即最多 16,384。限制后低于 `min` 时，这些档位都不可用。
- 夹紧后与低一档预算相同的档位不列出。例如输出上限为 8,192 时，只有 `low`（2,048）与 `medium`（4,096）。
- `max` 档不取模型的上限：models.dev 中 Claude 的预算上限是输出上限减 1，用满就不剩正文的空间；多数模型也没有写上限。

### 档位映射

| 协议 | 档位来自 `effort` 项 | 档位来自 `budget_tokens` 项 | 开启（toggle 项另加的 `high`） | 关闭（`off`） |
| --- | --- | --- | --- | --- |
| Chat Completions | 按 `effort_param` 写档位名 | 按 `thinking_budget` 写预算；`thinking_budget` 为 `none` 时这些档位不可用 | 按 `thinking_toggle` 写开启；`thinking_toggle` 为 `none` 时不可用 | 来自 toggle 项时按 `thinking_toggle` 写关闭，`thinking_toggle` 为 `none` 时不可用；来自 effort 项时按 `effort_param` 写 `"none"` |
| Responses | `reasoning: {"effort": "<档位>"}` | 不可用 | 不可用 | 来自 effort 项时 `reasoning: {"effort": "none"}`；来自 toggle 项时不可用 |
| Anthropic Messages | `thinking: {"type": "adaptive"}` 与 `output_config: {"effort": "<档位>"}` | `thinking: {"type": "enabled", "budget_tokens": N}` | `thinking: {"type": "adaptive"}` | `thinking: {"type": "disabled"}` |
| generateContent | `generationConfig.thinkingConfig.thinkingLevel` | `thinkingConfig.thinkingBudget: N` | `thinkingConfig.thinkingBudget: -1` | `thinkingConfig.thinkingBudget: 0` |
| Interactions | `generation_config.thinking_level` | 不可用 | 不可用 | 不可用 |

Chat Completions 中，模型的 `reasoning_options` 含 toggle 项且 `thinking_toggle` 不为 `none` 时，开启推理的档位（`off` 以外）同时按 `thinking_toggle` 写开启；没有 toggle 项的模型不写开关，例如 Kimi K3.1 Pro 只接受 `reasoning_effort`。某个档位在所选协议上不可用时，它不出现在可用档位中。

## 用量、缓存与费用

### 用量换算

每次尝试的用量按统一口径保存，各项都可以缺失；provider 没有报告的项为未知，不记为 0：

| 项 | 含义 |
| --- | --- |
| `input` | 全部输入 token，含缓存读与缓存写 |
| `cache_read` | `input` 中从缓存读取的部分 |
| `cache_write` | `input` 中写入缓存的部分 |
| `output` | 全部输出 token，含推理 |
| `reasoning` | `output` 中的推理部分 |

| 协议 | `input` | `cache_read` | `cache_write` | `output` | `reasoning` |
| --- | --- | --- | --- | --- | --- |
| Chat Completions | `prompt_tokens` | `prompt_tokens_details.cached_tokens`；DeepSeek 为 `prompt_cache_hit_tokens` | `prompt_tokens_details.cache_creation_input_tokens` | `completion_tokens` | `completion_tokens_details.reasoning_tokens` |
| Responses | `input_tokens` | `input_tokens_details.cached_tokens` | — | `output_tokens` | `output_tokens_details.reasoning_tokens` |
| Anthropic Messages | `input_tokens` + `cache_creation_input_tokens` + `cache_read_input_tokens` | `cache_read_input_tokens` | `cache_creation_input_tokens` | `output_tokens` | — |
| generateContent | `promptTokenCount` + `toolUsePromptTokenCount` | `cachedContentTokenCount` | — | `candidatesTokenCount` + `thoughtsTokenCount` | `thoughtsTokenCount` |
| Interactions | `total_input_tokens` + `total_tool_use_tokens` | `total_cached_tokens` | — | `total_output_tokens` + `total_thought_tokens` | `total_thought_tokens` |

- 解析端两种 cache_read 字段都读取，属于无歧义的兼容。
- 求和的项中有一项缺失时，和按已有的项计算；全部缺失时为未知。
- Interactions 的 `total_output_tokens` 是否含推理，Google 文档没有写明；表中按不含处理，实现时用真实响应核对，结论写回本表。
- 旧记录的 `prompt_tokens`、`completion_tokens` 读作 `input`、`output`，其余为未知。

### 缓存的开启与检测

| 协议 | 开启 | 检测 |
| --- | --- | --- |
| Chat Completions | 服务端自动缓存；`cache_markers` 为 true 时加显式断点 | `cache_read`、`cache_write` |
| Responses | 服务端自动缓存；请求带 `prompt_cache_key`，取 SessionId | `cache_read` |
| Anthropic Messages | 按 `cache` feature | `cache_read`、`cache_write` |
| generateContent、Interactions | 服务端隐式缓存，请求不加字段 | `cache_read` |

缓存键与断点只用 SessionId 和请求结构，不含用户内容。显式断点最多 4 个；Anthropic Messages 的系统提示断点与顶层自动断点合计 2 个，`cache_markers` 合计 2 个。

### 费用估算

每次请求结束时，请求网关按当时的模型价格估算这次尝试的费用，随用量交给调用方，保存在这次尝试上，标为估算。存下的是当时的估算值，之后价格变化（包括在 `providers.toml` 中修正）不回头修改旧记录；新版本之前的记录没有费用，显示为未知，不补算。价格是美元每百万 token：

```text
未命中输入 = input − cache_read − cache_write（缺失项按 0）
费用（美元）= ( 未命中输入 × cost.input
             + cache_read × (cost.cache_read，没有时用 cost.input)
             + cache_write × (cost.cache_write，没有时用 cost.input)
             + (output − reasoning) × cost.output
             + reasoning × (cost.reasoning，没有时用 cost.output)
             ) ÷ 1,000,000
```

- `input` 或 `output` 未知、或模型没有 `cost.input` 与 `cost.output` 时，这次尝试没有费用。
- 一轮的费用是各尝试之和；有尝试没有费用时，一轮的费用标为部分。
- 按上下文长度分档的价格（models.dev 的 `tiers`、`context_over_200k`）不计入，所以是估算。
- 费用只按模型价格计算，不因 provider 或认证方式区别；`chatgpt` 的模型取 `openai` 条目的价格，同样显示。

## 凭据

### `auth.json`

凭据保存在配置目录下的 `auth.json`（默认 `~/.lattice/auth.json`），只由后台读写：

```json
{
  "version": 1,
  "providers": {
    "deepseek": { "type": "api_key", "key": "<key>" },
    "chatgpt": {
      "type": "chatgpt",
      "host_id": "<host id>",
      "client_id": "<issued client id>",
      "account": { "subject": "<sub>", "email": "<email>" },
      "tokens": {
        "access_token": "<token>", "refresh_token": "<token>", "id_token": "<token>",
        "expires_at": "<RFC 3339>", "scopes": ["…"]
      }
    }
  }
}
```

- **权限。** 文件与所在目录属于当前用户，文件权限 0600。后台读取时发现属主不符、权限更宽或是符号链接，就拒绝使用并说明原因，不自动修改。
- **写入。** 后台进程内串行写入：先在同目录写临时文件（0600）并 fsync，再改名替换，最后 fsync 目录。
- **大小。** 文件最多 1 MiB；单个 key 1～16,384 字节，不含控制字符。
- **严格解析。** 不认识的 `version` 或结构错误时整个文件无效，所有需要凭据的操作按配置错误拒绝；`lattice auth status` 报告文件无效。
- **不外泄。** 凭据不进入会话库、profile 副本、日志、诊断、公开 DTO 或 Debug 输出。配置拒绝的脱敏以 `auth.json` 中的全部凭据值为已知值，见[会话合同的配置拒绝](session.md#配置拒绝的原因)。

### 凭据命令

`lattice auth` 是前端命令，只负责界面，经私有 IPC 交给后台执行：

| 命令 | 后台的动作 |
| --- | --- |
| `lattice auth set <provider>` | 从终端（不回显）或标准输入读取 key，校验后写入；key 不作为命令行参数 |
| `lattice auth remove <provider>` | 删除该 provider 的 key；对 `chatgpt` 先按[登出](#登出)处理，再删除 `client_id` 与账号信息，保留 host ID |
| `lattice auth login chatgpt` | SIWC 登录，见下节 |
| `lattice auth logout chatgpt` | SIWC 登出 |
| `lattice auth status` | 每个 provider 的凭据是否就绪；`chatgpt` 另给账号邮箱与 access token 的到期时间。不返回凭据本身 |

只有 `auth` 为 `bearer`、`x-api-key` 或 `x-goog-api-key` 的 provider 能 `set`。

原顶层命令 `lattice doctor` 只诊断环境变量与工作区 `.env` 中的凭据与 endpoint 来源，随这两个来源一起删除；凭据状态由 `lattice auth status` 给出，可用的 provider 与模型由 `lattice models` 给出。

## Sign in with ChatGPT

SIWC 按 OpenAI 的[开源工具接入方式](https://developers.openai.com/siwc/token-sharing-open-source)实现，只用于内置 provider `chatgpt`。

### 固定的 provider 与 endpoint

`chatgpt` 的协议、endpoint 与认证方式固定为表中的值：OpenAI 的接入文档只给出 `https://api.openai.com/v1`，token 的 audience 也是它。`providers.toml` 不能改写它们，见[用户配置](#用户配置-providerstoml)。

### 登录

登录在后台执行，前端只负责界面。整个登录是一次 IPC 交换，前端断开或取消即中止：

1. **准备。** 本机的 host ID 在首次登录前生成（随机 UUID）并保存，此后不变。后台为这次登录生成 `state`、OIDC `nonce` 与 PKCE verifier（S256），在 `127.0.0.1:1455` 启动回调监听，回调地址为 `http://127.0.0.1:1455/auth/callback`。端口被占用时登录失败并说明原因。
2. **授权 URL。** 后台把 `https://auth.openai.com/api/accounts/authorize` 的授权 URL 交给前端：`response_type=code`、`resource=https://api.openai.com/v1`、scope `openid profile email offline_access resource.invoke chatgpt.tokens.use.direct`、`ext_agent_host_id`、`state`、`nonce` 与 `code_challenge`。首次登录时 `client_id=dynamic_agent_client` 并带 `agent_name_hint=LATTICE`；已有注册时用保存的 `client_id`，带 `id_token_hint`（登出后没有保留则不带），不带 `agent_name_hint`。
3. **用户授权。** 前端打开浏览器或显示 URL。无头机器上，用户把浏览器最终跳转的 URL 粘贴给前端，前端转交后台，后台按回调同样处理。
4. **校验回调。** 先校验 `state`；回调带 `error` 时停止。首次登录的回调带回正式的 `client_id`，没有带回时登录不成立；再次登录的回调可以不带 `client_id`，带回与保存的不同时拒绝。
5. **换取 token。** 用正式的 `client_id`、`code`、`code_verifier` 与同一个 `redirect_uri`、`resource` 向 `https://auth.openai.com/api/accounts/oauth/token` 换取 token；`invalid_grant` 时丢弃这次登录。
6. **校验 id token。** 按 OpenAI 的 JWKS 校验签名，核对 issuer、audience（正式的 `client_id`）、有效期与 `nonce`；授予的 scope 必须含 `chatgpt.tokens.use.direct`。再次登录时，新 id token 的账号必须与保存的一致。
7. **保存。** 写入 `auth.json` 的 `chatgpt` 项，然后拉取[账号模型列表](#账号模型列表)，把结果返回前端。

### 刷新

- 有效期取 token 响应的 `expires_in`，换算成 `expires_at` 保存，不写死。
- 后台只在每次模型请求前检查：剩余有效期短于 300 秒（包括已经过期）时，先刷新再发请求。接受 Turn 时只确认已经登录，不检查有效期，也不刷新；不设定时刷新。
- 刷新用保存的 `client_id` 与 refresh token 请求 token 端点。成功后 access token、到期时间、scope 与新的 refresh token 一起写回 `auth.json`。
- 同一时刻需要刷新的多个请求只发起一次刷新，其余等它的结果。
- 刷新失败按原因区分：
  - token 端点返回 `invalid_grant`、`invalid_refresh_token`、`token_expired`、`refresh_token_expired`、`refresh_token_invalidated` 或 `refresh_token_reused`：refresh token 已失效。清除 token，保留 `client_id` 与 host ID；这次尝试按不可重试的认证错误停止，提示重新登录。
  - `invalid_client`：注册配置有误，按不可重试的错误停止，保留材料，提示重新登录（`lattice auth remove chatgpt` 后再登录会重新注册）。
  - 网络故障、超时与 5xx：凭据保留；这次尝试按可重试的 provider 错误停止，由协调层按现有重试预算重试，诊断写明是刷新失败。

### 登出

撤销 refresh token 后删除 access、refresh 与 id token，保留 `client_id`、账号信息与 host ID，供再次登录复用。撤销返回空的 HTTP 200 即成功；撤销未确认时仍删除这些 token，并告诉用户远端撤销未确认。

### 账号模型列表

- 用 access token 请求 `GET https://api.openai.com/v1/models`，保留 `visibility` 为 `list` 的项，以 `slug` 作为模型名、`display_name` 作为显示名。
- 在登录成功、后台的元数据刷新与 `lattice models refresh` 时拉取，结果保存在配置目录下的 `cache/chatgpt-models.json`。列表还没有时（例如刚登录后拉取失败），列出候选前先拉取一次。
- 推理档位与价格按同名模型取 `openai` 条目的元数据。

### 请求与错误

- 请求 `POST https://api.openai.com/v1/responses`，`store: false`、`stream: true`；只有收到 `response.completed` 才算完成，`response.incomplete`、`response.failed` 与流中断分别处理。
- 用量超限（`subscription_sharing_usage_limit_exceeded`，HTTP 429，也可能在流中途以 `response.failed` 给出）按不可重试的 provider 错误处理，诊断提示到 ChatGPT 设置查看用量。

## 限制

| 对象 | 限制 | 依据 |
| --- | --- | --- |
| `providers.toml` | 最多 1 MiB；provider 最多 256 个，每个 provider 手写的模型最多 1,024 个 | 实现常量，防止异常文件占用内存 |
| `auth.json` | 最多 1 MiB；key 1～16,384 字节 | 沿用现有连接材料的 key 上限 |
| models.dev 拉取 | 响应最多 32 MiB，30 秒内读完 | 2026-10-24 的 `api.json` 约 5.9 MB |
| SIWC 刷新余量 | 剩余有效期短于 300 秒时刷新 | access token 有效期 1 小时；余量覆盖发请求的时间与时钟偏差 |

Turn deadline 的范围由[会话合同](session.md#输入与传输限制)维护。

## 验证与承接

| 检查对象 | 入口 |
| --- | --- |
| 各协议的请求体与流式事件：文字、推理、多个工具调用、用量、错误事件 | `cargo test -p lattice-llm`，夹具取自官方文档 |
| 每个 feature 取值的请求体，默认值与标准协议形状一致 | 同上 |
| provider 注册表：内置表与 `providers.toml` 的合并、严格解析、`chatgpt` 不能改写、非 loopback 必须 HTTPS | 同上 |
| 元数据：来源的选择与按字段覆盖、只保留列出的字段、刷新失败继续使用上一份 | 同上 |
| 档位：可用档位的推导、按 token 数的预算、各协议的映射、不支持的档位被拒绝 | 同上 |
| 用量换算与费用估算，包括缺失项 | 同上 |
| `auth.json`：权限检查、原子写入、严格解析、凭据不进入日志与公开输出 | 同上 |
| endpoint 与凭据来源的负向测试：项目目录中的 `providers.toml` 不被读取，环境变量与工作区 `.env` 既不能改变 endpoint、也不能提供 key | harness 的 fast 场景 |
| SIWC 的本地逻辑：用注入的时钟与构造的短有效期 token 记录，核对剩余有效期短于余量（包括已经过期）时先刷新、有效期内不刷新，并发的请求只刷新一次，刷新失败按上文三类处理并写回 `auth.json`；回调的 `state` 与 `client_id` 校验，登录中途取消时关闭监听。不模拟 OpenAI 的授权服务 | `lattice-llm` 局部测试 |
| 真实 provider | 有凭据时验证 OpenAI（Chat Completions、Responses、推理、缓存）、ChatGPT 订阅（SIWC）、Gemini（generateContent、Interactions）；SIWC 与 OpenAI 的交互（登录、id token 校验、刷新、登出与账号模型列表）只由真实账号验证，CI 不覆盖；按 access token 的到期时间选时机，验证 Turn 中途的刷新，以及流跨过过期时刻会不会被断开。没有凭据的 provider 只按文档与夹具验证，交付时说明 |

内置表与 features 的依据：OpenAI [推理](https://developers.openai.com/api/docs/guides/reasoning)与[缓存](https://developers.openai.com/api/docs/guides/prompt-caching)，OpenAI SIWC [登录](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)、[账户与会话](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions)、[模型与推理](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference)、[错误与恢复](https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery)，Google [Interactions API](https://ai.google.dev/api/interactions-api) 与 [generateContent](https://ai.google.dev/api/generate-content)，xAI [推理](https://docs.x.ai/docs/guides/reasoning)，Mistral [推理](https://docs.mistral.ai/capabilities/reasoning/)，DeepSeek [思考模式](https://api-docs.deepseek.com/guides/thinking_mode)与[上下文缓存](https://api-docs.deepseek.com/guides/kv_cache)，Kimi [思考模型](https://platform.kimi.ai/docs/guide/use-kimi-k2-thinking-model)，Z.ai [思考模式](https://docs.z.ai/guides/capabilities/thinking-mode)，智谱 [思考模式](https://docs.bigmodel.cn/cn/guide/capabilities/thinking-mode)，阿里云 [深度思考](https://www.alibabacloud.com/help/en/model-studio/deep-thinking)、[上下文缓存](https://www.alibabacloud.com/help/en/model-studio/context-cache)与 [OpenAI 兼容](https://www.alibabacloud.com/help/en/model-studio/compatibility-of-openai-with-dashscope)，MiniMax [国际站](https://platform.minimax.io/docs/api-reference/text-anthropic-api)与[国内站](https://platform.minimax.cn/docs/api-reference/text-anthropic-api)，硅基流动 [推理](https://docs.siliconflow.cn/cn/userguide/capabilities/reasoning)，OpenRouter [推理](https://openrouter.ai/docs/use-cases/reasoning-tokens)，Groq [推理](https://console.groq.com/docs/reasoning)，均于 2026-10-25 查阅。火山方舟的推理参数没有取得官方文档，`volcengine` 的 features 与 Anthropic 兼容厂商的缓存支持在实现时核对，结论写回本表。

决策理由见 [ADR 0010](../adr/0010-protocol-adapters-and-configured-providers.md)；局部细则由 [`lattice-llm`](../../crates/lattice-llm/AGENTS.md) 入口承接。
