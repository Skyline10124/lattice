---
status: accepted
---

# ADR 0010：模型接入按协议适配，provider 改为配置

- 状态：Accepted（2026-10-25 维护者批准，记录见 #316）
- 日期：2026-10-24
- 修订：细化 ADR 0005 第 7 节“同 provider 回放”中来源的粒度；部分取代 ADR 0006 第 4 节中“当前 agent 即当前位置所属 Turn 的 agent”的规则，以及 ADR 0008 第 10 节中由客户端读取配置与凭据的规则。范围见[与 ADR 0005、ADR 0006、ADR 0007、ADR 0008 的关系](#与-adr-0005adr-0006adr-0007adr-0008-的关系)
- 依赖：ADR 0007（接受时确定的模型与 provider 记在 UserMessage 节点上）
- 实施跟踪：GitHub #315
- 修正：2026-10-25 起草合同（#317）时，维护者裁决后台直接使用启动环境、删除 `LATTICE_AGENTS_DIR`，SIWC 不设模拟授权服务的测试，`lattice-llm` 对外改为请求网关、profile 副本只记 provider 与模型的 `id`，费用由请求网关在请求结束时估算并保存；第 1、4、6、8、11、13、15、17 节，后果，否决的方案与“不在本文决定的事项”随之修正

## 背景

`lattice-llm` 把模型请求发给 provider，并把流式响应交给回复组装。以下按 `main` 的 `a90e4c0a` 核对。

- **适配按厂商划分。** `ProviderKind` 有 `OpenAi`、`OpenAiCompatible`、`Anthropic`、`Gemini` 四个值（`crates/lattice-llm/src/router.rs`）。`client.rs` 按它决定认证头、请求路径和请求构造，`connection.rs`、`lattice-api` 的冻结配置也带着它。同一协议的不同厂商只能共用一个 `OpenAiCompatible`。
- **模型靠写死的别名表选择。** 可选的只有 `gpt-5.5`、`gpt-5.4`、`gpt-5.3-codex`、`gpt-4o-mini`、`opus`、`sonnet`、`haiku`、`gemini`，以及 `compat:<模型>`。新模型上线或旧模型下线都要改代码：#269 中 `gemini` 指向的模型对新用户已不可用；`gpt-5.3-codex` 只能走 Responses API，按现有路径请求返回 404（#271 的评论）。
- **会话中不能切换模型。** 模型只在 profile 中设定，CLI 的 `session send` 只能换 agent，TUI 没有切换模型或推理强度的入口。
- **每次提交都重读 profile。** 提交端在每次提交前从磁盘读取所选 agent 的 `agent.toml` 并解析外部策略文件，冻结进这一轮；会话本身不保存配置，当前 agent 是当前位置所属 Turn 的 agent。改动 `agent.toml` 会改变已有会话下一轮的行为。
- **配置与凭据由提交端读取。** CLI 或 TUI 进程在提交前读取 profile、解析模型与 endpoint，从自己的环境变量或工作区 `.env` 读取 key，冻结进连接材料，经私有 IPC 交给后台（`crates/lattice-api/src/turn/live.rs`，ADR 0008 第 10 节）。R1 引入后台（`19e388fb`）时，配置加载留在了提交端。后台启动时不继承前端的环境；凭据只在内存与私有 IPC 中流转，不持久化。
- **endpoint 与凭据每家固定一套。** 每个 provider 一个固定的 key 环境变量；OpenAI 兼容只能配置一个 endpoint；官方 OpenAI 的 endpoint 不能改写。
- **厂商差异散在代码里。** 例如 Chat Completions 解析同时读取 `reasoning_content` 与 `reasoning` 两个字段；Gemini 的工具 schema 改用 `parametersJsonSchema`（#270）。这些差异不能按 provider 配置，新增一家有差异的兼容厂商就要加分支。
- **没有模型元数据。** 不知道上下文窗口、输出上限和推理能力。Anthropic 的 `max_tokens` 用固定默认值；请求从不带推理参数，`gpt-6-luna` 在 Chat Completions 上不关闭推理就不能使用工具（#226 的评论）。
- **用量只有三项。** `TokenUsage` 只有 `prompt_tokens`、`completion_tokens`、`total_tokens`，按尝试保存在会话库中；推理与缓存 token 不区分，请求也不开启缓存。
- **回放来源按协议记录。** `ProviderOrigin` 只有 `OpenAiCompatible`、`Anthropic`、`Gemini`，所有 OpenAI 兼容厂商共用一个来源。

外部条件也在变化：

- OpenAI 的新模型和部分能力只在 Responses API 上提供。
- Google 的 Interactions API 于 2026 年 6 月 GA，是 Google 推荐的接口，新模型与新功能先在这里上线；`generateContent` 仍完整支持。
- OpenAI 于 2026-09-29 推出 Sign in with ChatGPT（SIWC），开源与本地运行的工具可以让用户用 ChatGPT 订阅额度调用 Responses API。它要求客户端持久保存登录材料，access token 的有效期为 1 小时，与现在 Turn deadline 的上限（`[llm].timeout_secs` 最大 3600 秒）相同。

## 决策

### 1. 适配按协议划分

协议适配器负责一种线上协议的请求构造与流式事件解析。只有以下五个，全部使用流式端点：

| 协议 | 端点 |
| --- | --- |
| Chat Completions | `POST {base_url}/chat/completions`，`stream: true` |
| Responses | `POST {base_url}/responses`，`stream: true` |
| Anthropic Messages | `POST {base_url}/v1/messages`，`stream: true` |
| Google generateContent | `POST {base_url}/models/{model}:streamGenerateContent?alt=sse` |
| Google Interactions | `POST {base_url}/interactions`，`stream: true` |

回复组装不随协议变化：适配器把线上事件翻译为部件增量，由 #312 的累积器组装完整回复。

`ProviderKind` 删除。增加协议需要新的 ADR 或对本 ADR 的修订。

`lattice-llm` 对外只提供请求网关：调用方给出 provider `id`、模型 `id`、推理档位、对话与工具定义，得到流式事件、用量与费用估算。provider 的解析、凭据（含 SIWC 的刷新）、协议适配与传输都在网关内部完成，调用方不接触 endpoint、key 与 features。网关每次请求按当前的配置与凭据解析，不按 Turn 冻结它们。

### 2. provider 是配置

provider 由以下字段描述，不对应单独的代码：

| 字段 | 含义 |
| --- | --- |
| `id` | provider 标识，用于模型引用、节点记录、回放来源与凭据查找 |
| `protocol` | 第 1 节的五个协议之一 |
| `base_url` | endpoint |
| `auth` | 认证方式：`bearer`、`x-api-key`、`x-goog-api-key`、`oauth:chatgpt` 或 `none`（本地服务），默认值随协议 |
| `features` | 第 3 节 |
| `catalog` | 对应的 models.dev provider 条目，用于取模型元数据 |

官方厂商与兼容厂商用同一种描述。例如 DeepSeek 是 `protocol = "chat-completions"`、`base_url = "https://api.deepseek.com"`，再加上它需要的 features。凭据不属于 provider 配置，按 provider `id` 保存在 `auth.json` 中（第 4 节）。

### 3. 厂商差异用 features 表达

feature 是某个协议适配器支持的一项可选行为。

- **按协议划分、封闭。** 每个协议的 feature 集合由该协议适配器的代码定义；配置只能在已有取值中选择，不能描述任意的请求改写。
- **取值是布尔量或有限枚举。** 例如 Chat Completions 的推理参数格式：`reasoning_effort`、`reasoning.effort`、`thinking.type` 等。
- **默认值等于标准协议行为。** 未设置任何 feature 的 provider 按协议文档的标准形状请求。
- **不按 URL 探测厂商。** 厂商的行为只由它的 features 决定。
- **新的厂商差异先加 feature。** 在协议适配器中增加一个 feature 取值并测试，再在 provider 表中启用。
- **provider 级设置，可按模型覆盖。** 同一 provider 的不同模型行为不同时，在模型上覆盖。
- **解析端对无歧义的差异直接兼容。** 例如两种 reasoning 字段名都读取。features 约束的是请求构造与回放。

各协议的 feature 清单由合同维护，见第 15 节。

### 4. 全局配置与凭据

provider 配置、凭据与模型元数据是用户级的全局状态，不随提交它的 shell、项目或前端变化。key 会发到 endpoint，所以 endpoint 只来自经过审查或用户自己写的配置。

- **内置 provider 表。** 手工维护，提交进仓库，编译进程序。每一家的协议、endpoint、认证方式与 features 对照厂商文档核对，变化在 PR 中审查；models.dev 只通过 `catalog` 字段提供模型元数据。第一轮收录 20 家常用 provider：

  | 分组 | provider |
  | --- | --- |
  | 官方 | `openai`、`anthropic`、`google`、`xai`、`mistral` |
  | 国内 | `deepseek`、`moonshotai`、`moonshotai-cn`、`zai`、`zhipuai`、`alibaba`、`alibaba-cn`、`minimax`、`minimax-cn`、`volcengine`、`siliconflow-cn` |
  | 聚合 | `openrouter`、`groq` |
  | 本地 | `ollama`、`lmstudio`（`auth = "none"`） |

  国际站与国内站 endpoint 不同的厂商各算一家。表中不记录某一家是否经过实测；实际验证范围写在实现 PR 的验证记录中。SIWC 另用内置 provider `chatgpt`，见第 11 节。
- **用户全局 `~/.lattice/providers.toml`。** 新增 provider，或覆盖内置 provider 的字段（`chatgpt` 的限制见第 11 节）。一个 provider 一张表，`models` 子表按模型补充元数据，`features` 子表设置厂商差异，例如：

  ```toml
  [providers.my-vllm]                   # 新增自建服务
  protocol = "chat-completions"
  base_url = "https://vllm.example.com/v1"

  [providers.my-vllm.models."qwen3-32b"] # 元数据中没有的模型，手写元数据
  context = 131072
  output = 8192

  [providers.deepseek]                  # 覆盖内置 provider 的 endpoint
  base_url = "https://gateway.example.com/deepseek"

  [providers.deepseek.features]
  thinking_format = "thinking.type"
  ```

  完整字段由合同规定。
- **凭据保存在 `~/.lattice/auth.json`。** API key 按 provider `id` 保存，SIWC 的登录材料也在这里（第 11 节）。文件权限 0600，只由后台读写。
  - `lattice auth` 是前端命令，只负责界面：录入与删除 key、登录与登出都经私有 IPC 交给后台执行。key 从终端或标准输入读取，不作为命令行参数。
  - 状态查询只返回每个 provider 的凭据是否就绪，不返回凭据本身；`chatgpt` 另给出 access token 的到期时间。
  - 写入在后台进程内串行，先写临时文件再改名替换，后台中途退出也不会留下写了一半的文件。
  - 配置文件中不写 key；凭据不写入会话库、profile 副本、日志、诊断或公开输出。
- **环境变量与项目目录不提供配置。**
  - 项目目录随仓库检出，其中的配置不可信，不读取其中的 `providers.toml`。agent profile 仍按现有规则可以放在项目内，它只能选择 `<provider>/<model>`，不能改变 endpoint 或认证方式。
  - 环境变量与工作区 `.env` 不再提供 key、endpoint 与模型：`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`GEMINI_API_KEY`、`OPENAI_COMPATIBLE_BASE_URL`、`OPENAI_COMPATIBLE_MODEL`、`OPENAI_COMPATIBLE_API_KEY`、`ANTHROPIC_BASE_URL`、`GEMINI_BASE_URL` 删除。工作区 `.env` 的正当用途只是提供 provider key（`crates/lattice-cli/src/env_boundary.rs`），它的加载随之删除。
  - 内置的 `openai-compatible` provider 与 `compat:<模型>` 写法一并删除。本地服务使用内置的 `ollama`、`lmstudio`，或在 `providers.toml` 中配置。
- **由后台读取。** 内置表、`providers.toml`、元数据与 `auth.json` 由后台读取，agent profile 也由后台按现有查找规则（先项目内 `.lattice/agents`，再全局目录）读取。提交端只交消息、工作区与用户的选择（第 13 节）。后台在接受时解析配置、读取凭据并冻结进这一轮：API key 随这一轮冻结在后台内存中，SIWC 的 access token 由后台在执行中按需刷新（第 11 节）。配置错误由后台在接受前拒绝，诊断随拒绝带回前端。
- **配置目录与 TLS 根证书由后台的启动环境确定。** 后台使用启动它的进程的环境，启动时读一次，不清空、不替换；显式启动与前端自动启动行为相同，自动启动的后台继承拉起它的前端的环境。配置目录为 `$HOME/.lattice`，TLS 根证书按 `rustls-native-certs` 的规则从这份环境加载。此后不随提交变化，连接到同一后台的前端共用这份配置；请求不带配置来源，环境变化在重启后台后生效。
- **agents 目录固定。** 项目内的 `.lattice/agents` 与配置目录下的 `agents`，`LATTICE_AGENTS_DIR` 删除。
- **Git 库一律隔离。** 后台不再替换 `HOME`，所以创建或打开 Git 库时一律使用隔离方式，不读用户与全局的 Git 配置。
- **非 loopback 的 endpoint 必须使用 HTTPS。** 沿用现有规则，对内置表与 `providers.toml` 同样适用。
- **元数据不提供 endpoint。** 第 6 节的元数据（内置快照与缓存）中即使有 endpoint、环境变量名或协议字段也不使用。

这改变了现行的凭据边界：凭据原来由提交端从环境读取、只在内存与私有 IPC 中流转，现在保存在 `auth.json`，由后台读取。

provider 配置不写入会话，也不按 Turn 冻结：请求网关每次请求按当前配置解析（第 1 节），`providers.toml` 的修改从下一次请求起对所有会话生效。

### 5. 模型写作 `<provider>/<model>`

- profile 的 `[agent].model` 与会话中的切换都写作 `<provider>/<model>`，例如 `anthropic/claude-sonnet-4-6`、`deepseek/deepseek-v4-pro`。
- 在第一个 `/` 处拆分：provider `id` 不含 `/`，模型名可以含 `/`，例如 `openrouter/anthropic/claude-sonnet-4.6`。
- 写死的别名表整张删除，不保留便捷别名，也不设默认模型。
- 会话中切换模型与推理强度：CLI 用 `--model`、`--effort` 参数，TUI 用第 14 节的命令；选择写回会话的 profile 副本（第 13 节）。
- UserMessage 节点上的 `provider` 记 provider `id`，`model` 记模型名。

### 6. 模型元数据取自 models.dev

- 元数据包括上下文窗口、输出上限、推理档位、工具支持、输入模态与价格。它只影响请求参数与显示，不影响请求发往哪里（第 4 节）。
- **内置快照。** xtask 从 models.dev 生成内置 provider 所对应条目的元数据快照，提交进仓库审查并编译进程序。首次运行与离线时使用它。
- **定时刷新的缓存。** 后台进程启动时与此后每 24 小时从 `https://models.dev/api.json` 拉取一次，写入 `~/.lattice/cache/models.json`。拉取失败只记日志，继续使用上一份数据。`lattice models refresh` 立即刷新一次。缓存没有过期的概念：数据旧了只影响列表与价格的准确性，不影响请求发往哪里。
- **取值优先级。** `providers.toml` 中手写的元数据优先，其次是缓存，再次是内置快照。
- **请求路径上不联网。** 接受 Turn 与每次请求时只读上述数据。
- **没有元数据的模型仍可使用。** 只是没有价格与推理档位。

### 7. 推理强度

- 统一档位：`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`，`max` 在 `xhigh` 之上。
- 模型可用的档位由元数据给出（models.dev 的 `reasoning_options`），协议适配器把档位映射为该协议的参数。
- 选了模型不支持的档位时，在接受前按配置错误拒绝，不降级到相近档位。
- profile 的 `[agent].effort` 设默认档位，会话中可以切换（第 13 节）。不设档位时不发送推理参数，由 provider 使用默认行为。生效的档位随 Turn 冻结并记在 UserMessage 节点上。

### 8. 用量细分、费用与缓存

- 每次尝试的用量细分为输入、缓存读、缓存写、输出与其中的推理。各协议的报告口径不同（例如 OpenAI 的输入含缓存命中，Anthropic 不含），由协议适配器换算为统一口径，换算规则写入合同。
- provider 没有报告的细分项为未知，不记为 0，与现有“缺失的用量为未知”一致。
- 每次请求结束时，请求网关按当时的模型价格估算费用，随用量一起保存在这次尝试上，标为估算；没有价格时这次尝试没有费用。存下的是当时的估算值，之后价格变化不回头修改；新版本之前的记录没有费用。费用只按模型价格计算，不因 provider 或认证方式区别，SIWC 同样显示。
- 用量在两处查看：TUI 的 statusline 显示本轮的 token、缓存命中与费用估算；`lattice session usage <SESSION_ID>` 逐轮列出用量与费用估算并给出合计，支持 `--json`。
- 缓存的开启与检测一起做：适配器按协议开启缓存（Anthropic 的 `cache_control`，OpenAI 的 `prompt_cache_key` 等），显示每轮的缓存命中。缓存键不含用户内容。

### 9. 不使用服务端会话状态

会话树是会话事实的唯一权威（ADR 0007）。

- Responses 与 Interactions 固定发送 `store: false`，每次请求发送当前路径上的完整历史。
- 不使用 `previous_response_id`、`previous_interaction_id`、后台执行等依赖服务端状态的能力。

这也避免对话内容默认留存在 provider 处：Interactions 的 `store` 默认为 `true`，付费层保留 55 天。

### 10. 回放来源记为“协议 + provider”

签名、加密 reasoning 等不透明回放材料只对产生它的 provider 与协议有效。同一协议的另一家厂商不一定接受，同一厂商的另一个协议也不一定接受。

- 回放来源从协议（现有 `ProviderOrigin`）改为“协议 + provider `id`”。
- 来源相同时保留不透明回放材料；来源不同时按跨来源规则投影：去除来源 reasoning、provider call ID 与回放状态，只保留中立语义。

### 11. Sign in with ChatGPT

SIWC 是一种认证方式：`protocol = "responses"`、`auth = "oauth:chatgpt"`，Responses 适配器本就固定发送 `store: false`（第 9 节），也不发送 `temperature`、`max_output_tokens` 等参数（见 `llm.md`），满足 SIWC 的要求，所以它既不需要单独的协议适配器，也不需要专用的 feature。

- **固定的 provider 与 endpoint。** SIWC 只用内置 provider `chatgpt`，endpoint 固定为 `https://api.openai.com/v1`：OpenAI 的接入文档只给出这一个 endpoint，token 的 audience 也是它。`providers.toml` 不能覆盖 `chatgpt` 的协议、endpoint 与认证方式，也不能给其他 provider 设置 `oauth:chatgpt`；违反时在接受前按配置错误拒绝。推理档位与价格取 models.dev 中 `openai` 条目的元数据。
- **登录在后台执行。** 按 OpenAI 开源工具接入方式完成授权码加 PKCE 流程，前端只负责界面：
  1. 后台生成 state、nonce 与 PKCE 参数，在 `127.0.0.1:1455` 启动回调监听，把授权 URL 交给前端。首次登录时，URL 中的 `client_id` 是注册入口 `dynamic_agent_client`，它不保存、也不用于换取 token；再次登录时用已保存的 `client_id`。
  2. 前端打开浏览器或显示 URL。无头机器上，用户把浏览器最终跳转的 URL 粘贴给前端，由前端转交后台。
  3. 后台校验回调。首次登录的回调带回正式的 `client_id`，后台保存它，再用它换取 token；没有带回时登录不成立。再次登录的回调可能不带 `client_id`，沿用已保存的值；带回不同的值时拒绝。
  4. 后台写入 `auth.json`，并拉取账号的模型列表。
  5. 前端取消或断开时，后台中止这次登录并关闭监听。

  本机的 host ID 在首次登录前生成并保存，此后保持不变。再次登录复用同一个 `client_id` 与 host ID，不重新注册。
- **登录材料。** `client_id`、host ID 与 access、refresh、id token 保存在 `auth.json`（第 4 节）。
- **按需刷新。**
  - 后台只在每次模型请求前检查 access token 的剩余有效期，短于余量（包括已经过期）时先刷新再发请求，所以 Turn 的时长不受 token 有效期限制。不设定时刷新。余量由合同规定。
  - 接受 Turn 时只确认已经登录，不检查有效期，也不刷新。
  - 有效期取 token 响应中的 `expires_in`（现为 1 小时），不写死。
  - refresh token 每次刷新都会更换，刷新结果立即写回 `auth.json`。同一时刻需要刷新的多个 Turn 只发起一次刷新，其余等待它的结果。
  - 刷新失败按原因区分：
    - token 端点确认 refresh token 已失效（例如 `invalid_grant`、`refresh_token_expired`、`refresh_token_reused`）时，清除失效的 token，这次尝试按认证错误停止，提示重新登录；`client_id` 与 host ID 保留。
    - 网络故障、服务端临时错误等不代表凭据失效，保留凭据与真实的错误原因，恢复方式由合同规定。
- **登出在后台执行。** 撤销 refresh token 后删除本地的 access、refresh 与 id token，保留 `client_id` 与 host ID 供再次登录。撤销未确认时仍删除这些 token，并提示用户远端撤销未确认。
- **模型列表。** `chatgpt` 可用的模型以 `GET /v1/models` 的结果为准，由后台拉取并保存，随第 6 节的元数据刷新与 `lattice models refresh` 更新；还没有列表时（例如刚登录），列出候选前先拉取。推理档位与价格仍取第 6 节的元数据。
- **用量超限。** SIWC 的用量超限（例如 `subscription_sharing_usage_limit_exceeded`，可能在流中途出现）按不可重试的 provider 错误处理。
- **Turn deadline 上限提高到 24 小时。** 对所有 provider 适用：
  - `[llm].timeout_secs` 的上限从 3600 秒改为 86400 秒，默认 300 秒不变。超过 token 有效期的 Turn 由此成为实际用法。
  - HTTP 请求的兜底总时限现与原上限一致（3600 秒），随之调整。
  - deadline 的粒度、默认值与到点后的结局仍由 #222 裁决。

### 12. Google 内置 provider 的默认协议

Interactions 适配器上线后，内置 `google` provider 默认使用 Interactions；`generateContent` 保留，可在 `providers.toml` 中切换。

### 13. 会话持有 profile 副本

会话建立时取一份 profile 副本，此后只用副本，不再读取原 profile。

- **创建。** 新会话首次提交时，提交端交所选 agent 的名字，以及可选的模型与推理档位；后台读取该 agent 的 profile（解析外部策略文件、加载 system prompt），核对所选的 provider 与模型可用，在接受事务中写成这个会话的 profile 副本。
- **内容。** 有效 profile、当前的 provider `id` 与模型 `id`、推理档位。不含 provider 配置与凭据，它们由请求网关每次请求时取得（第 1 节）。
- **使用。** 已有会话的后续提交只用副本，不再读取原 `agent.toml`。模型请求经请求网关按副本中的 provider `id` 与模型 `id` 发出。
- **修改。** `/model`、`/effort` 与 CLI 的 `--model`、`--effort` 选择的值随下一次提交交给后台，在接受事务中写回副本。`--agent` 换 agent 时，后台用该 agent 的 profile 重新生成副本。
- **选择随提交生效。** 选择后未发送就退出，这次选择不保留。所以副本中的模型与档位总是最后一轮实际使用的，重新打开会话时沿用最后一轮的模型与档位。
- **rewind 不改变副本。** 副本属于会话，不属于节点；当前 agent 是副本中的 agent。每一轮实际使用的 agent、模型、provider 与档位仍记在它的 UserMessage 节点上。
- **原 profile 的修改只对新会话生效。** 包括收紧的 LANCET 策略（写权限、网络白名单）；要让已有会话使用新策略，需要开新会话。

### 14. TUI 的 `/model` 与 `/effort`

TUI 用 slash 命令选择模型与推理强度，选择界面是 Dock 中的面板。

- **面板可以交互。** 现有面板只显示 `/help` 的输出；选择面板在 Dock 中列出候选项，输入文字过滤，↑↓ 移动，Enter 选定，Esc 取消。焦点在面板时由面板解释按键。
- **`/model`。** 列出可用的 `<provider>/<model>`：provider 来自内置表与 `providers.toml`；模型来自第 6 节的元数据，取值优先级相同，没有缓存时使用内置快照；`chatgpt` 的模型只取第 11 节的账号模型列表。候选项标明凭据是否就绪。
- **`/effort`。** 只列出当前所选模型支持的档位。模型没有元数据时，面板说明该模型不能选择推理强度，可以在 `providers.toml` 中为它手写档位。
- **切换模型时。** 当前档位新模型不支持时，改回副本中 profile 的默认档位；默认档位也不支持时清除，由状态消息说明。
- **作用范围。** 选择由 TUI 进程持有，随下一次提交写回会话的 profile 副本，不写回原 profile（第 13 节）。
- **statusline。** 显示下一次提交将使用的模型、provider 与档位：有未提交的选择时显示所选值，否则显示副本中的值。

候选列表由后台的查询给出：后台按第 4、6、11 节列出可用的模型、各自支持的档位与凭据是否就绪。这是 ADR 0008 的查询之外新增的一个查询；CLI 列出模型的命令使用同一个查询。

### 15. 合同先行

实施前先完成以下合同修订并获批准：

- 新增 `docs/architecture/llm.md`，作为协议、provider 配置、features 清单、元数据、推理档位映射、用量换算、凭据保存与订阅登录规则的唯一出处。
- `docs/architecture/session.md`：配置由后台读取，提交只带消息、工作区与选择，冻结配置表与配置错误的展示随之修改（第 4 节）；凭据操作与登录的后台接口（第 4、11 节）；Turn deadline 上限与 HTTP 兜底时限（第 11 节）；会话的 profile 副本、当前 agent 的判定与提交流程（第 13 节），会话头新增的副本摘要；配置安全边界与凭据边界（第 4、11 节）；回放与跨 provider 投影（第 10 节）；用量记录（第 8 节）；模型候选查询（第 14 节）。
- `docs/architecture/tui.md`：打开已有会话不再加载 profile，可交互的面板、`/model` 与 `/effort`、候选列表来自后台查询、statusline 字段（第 13、14 节）。
- `crates/lattice-cli/AGENTS.md`：删除工作区 `.env` 的环境边界，新增 `lattice auth`（第 4、11 节）。
- `docs/development/testing.md` 与 `crates/lattice-system-harness/AGENTS.md`：fake provider 改为经测试目录中的 `providers.toml` 与 `auth.json` 接入，不再经环境变量；测试 CA 经后台的启动环境生效（第 4 节）。

### 16. 旧数据

- 旧会话可以继续读取与续接。节点上旧的 provider 值映射到内置 provider：`openai`、`anthropic` 保持不变，`gemini` 映射为 `google`；`openai_compatible` 无法确定是哪一家，按未知来源处理，续接时按跨来源规则投影。
- 旧的用量记录中 `prompt_tokens`、`completion_tokens` 读作输入与输出，缓存与推理细分为未知。OpenAI 兼容的旧记录中输入含缓存命中，已无法拆分。
- 旧会话没有 profile 副本。升级后第一次提交时，后台按当前位置所属 Turn 的 agent 读取 `agent.toml`，生成副本。
- 新版本写入的记录，旧版本无法读取。

### 17. 验证方式

- 每个协议适配器有基于官方文档的请求体与流式事件夹具，覆盖文字、推理、多个工具调用、用量与错误事件。
- 每个 feature 取值有请求体断言，默认值与标准协议形状一致。
- 元数据的取值优先级与刷新：手写值、缓存与内置快照的覆盖顺序；后台刷新失败时继续使用上一份数据。
- endpoint 与凭据来源的负向测试：项目目录中的 `providers.toml` 不被读取，元数据缓存中的 endpoint 不被使用，环境变量与工作区 `.env` 既不能改变 endpoint，也不能提供 key。
- 配置由后台读取：从环境变量不同的两个 shell 提交到同一后台，使用同一份配置与凭据；配置错误在接受前拒绝，诊断带回前端；凭据不出现在会话库、profile 副本、日志与公开输出中，`auth.json` 的权限为 0600。
- SIWC 的配置：`providers.toml` 改写 `chatgpt` 的 endpoint，或给其他 provider 设置 `oauth:chatgpt` 时，在接受前被拒绝。
- SIWC 的刷新逻辑由 `lattice-llm` 局部测试验证：注入时钟、构造短有效期的 token 记录，覆盖剩余有效期短于余量时先刷新、有效期内不刷新、同时需要刷新的多个请求只刷新一次、refresh token 被判失效时清除 token 并按认证错误停止、token 端点暂时不可用时保留凭据。不模拟 OpenAI 的授权服务：照文档写的假服务证明不了与真实服务一致，与 OpenAI 的交互只由真实账号验证。
- SIWC 的登录：首次登录用 `dynamic_agent_client` 打开授权页，保存回调带回的 `client_id`；再次登录的回调带回不同的 `client_id` 时拒绝；前端不直接写 `auth.json`；登录中途取消时，后台关闭回调监听；登出后 `client_id` 与 host ID 保留，再次登录复用它们。
- TUI 的选择面板由 pty 场景覆盖选择、过滤与取消；选择后提交的请求体带上所选模型与档位。没有元数据缓存时 `/model` 列出内置快照中的模型，`chatgpt` 只列出账号模型列表中的模型。
- 已有会话在原 `agent.toml` 修改或删除后仍按副本执行；切换模型后重新打开会话，沿用最后一轮的模型与档位；rewind 不改变副本。
- 有凭据时用真实 provider 验证：OpenAI API key（Chat Completions、Responses、推理、缓存），ChatGPT 订阅账号（SIWC），Gemini API key（generateContent、Interactions）。没有凭据的 provider 只按文档与夹具验证，并在交付时说明。
- SIWC 的真实账号按时机验证，不需要超过 1 小时的 Turn：
  - 跨刷新：access token 的剩余有效期略长于余量时，提交一轮多步 Turn，让后面某一步的请求前触发刷新；这一轮正常完成，`auth.json` 中的 refresh token 已经更换。
  - 跨过期：剩余有效期只比余量多几十秒时，发一个输出很长的请求，让流持续到过期之后，确认 provider 会不会断开它。

## 与 ADR 0005、ADR 0006、ADR 0007、ADR 0008 的关系

- **ADR 0005 第 7 节。** 同 provider 回放可以使用不透明回放材料，跨 provider 投影只使用中立语义。本 ADR 保留这条规则，把其中的“provider”细化为“协议 + provider `id`”（第 10 节）。
- **ADR 0006 第 4 节。** “不设 Session 级当前 agent 字段，当前 agent 即当前位置所属 Turn 的 agent，rewind 后回到当时的 agent”由第 13 节取代：当前 agent 是会话 profile 副本中的 agent，rewind 不改变它。`send --agent` 仍可换 agent，改为重新生成副本。
- **ADR 0007 第 6 节。** 接受时确定的事实仍记在 UserMessage 节点上，配置表不变。该节删除的是每一轮各存一份的 profile 快照与策略快照，理由是它们没有读者、内容与节点重复。本 ADR 的副本每个会话一份，是后续提交的配置来源，两者不冲突。
- **ADR 0008 第 10 节。** “读取 profile、系统提示与连接凭据，加载好之后提交一次；凭据来自前端进程的环境与 `.env`”由第 4、13 节取代：客户端只提交消息、工作区与选择，配置与凭据由后台读取。该节其余各项（连接后台、未运行时启动它，提交与取消，连接中断提示，分帧）不变。凭据的录入与删除、登录与登出（第 4、11 节）加入 ADR 0008 的命令；凭据状态与第 14 节的模型候选加入其查询。

四份 ADR 的其余内容不受影响。

## 后果

**收益**

- 接入一家使用已有协议的厂商只需写配置；常见厂商由内置表直接提供，用户只需用 `lattice auth` 录入 key。
- 配置与凭据只在后台一处读取，前端只交选择；同一后台上的会话不会因为提交它的 shell 不同而用到不同的配置。
- 厂商差异集中在协议适配器的 features 中，每项有名字和测试，不再散在代码分支里。
- 模型随 provider 上线即可使用，不再依赖别名表更新。
- 推理强度、用量细分、缓存与费用估算有了统一的来源与口径。
- Responses、Interactions 与 SIWC 共用同一套 provider 与组装机制；SIWC 的 token 在执行中刷新，Turn 的时长不受 token 有效期限制。

**代价**

- 破坏性变化：
  - 现有 profile 中的别名失效，须改为 `<provider>/<model>`。
  - 环境变量与工作区 `.env` 中的 key 不再生效，须用 `lattice auth` 录入。
  - 用环境变量配置 endpoint 或 OpenAI 兼容服务的用户须改写为 `providers.toml`。
  - 会话库记录格式变化，旧版本读不了新库。
- API key 与 SIWC 登录材料保存在磁盘上的 `auth.json`，这是一类需要保护的本地文件；后台进程持有凭据。
- 接受 Turn 时不刷新 token，登录失效要到这一轮第一次需要刷新时才发现，这一轮以认证错误结束。
- Turn deadline 上限提高后，配置错误的超长 deadline 最多让一轮跑 24 小时才停。
- SIWC 与 OpenAI 授权服务的交互没有自动化测试，回归只能在真实账号验证时发现。
- 配置目录与 TLS 根证书由后台的启动环境确定，改动环境要重启后台才生效。
- provider 配置不按 Turn 冻结，修改从下一次请求起生效；执行中的一轮可能前后两次请求发往不同的 endpoint。
- harness 的 fake provider 目前经环境变量接入，要随之迁移。
- 内置表需要手工维护；厂商改了 endpoint 或协议细节，要发新版本更新。内置表之外的 provider 由用户在 `providers.toml` 中自行添加。
- 后台每天访问一次 models.dev。
- feature 集合会随接入的厂商增长。
- 修改 `agent.toml`（包括收紧 LANCET 策略）不再影响已有会话，要开新会话才生效。

## 否决的方案

- **继续按厂商写适配器。** 兼容厂商越多，分支越多，同一协议的修复要在多处重复。
- **按 URL 探测厂商行为。** 经代理或网关访问时会判断错误，行为也不可见。
- **用运行时元数据提供 endpoint。** 元数据源被篡改或网络路径被劫持时，key 会被发往别处。
- **按 models.dev 生成内置表、全部收录（205 家）。** 绝大多数没有人核对过协议与厂商差异，按默认 features 发请求很可能出错；内置表只收常用的，并逐家核对。
- **元数据缓存设过期期限，过期后提示或停用。** 提示不改变行为，可以一直忽略；停用会让离线时不可用。改为定时刷新，旧数据只影响准确性。
- **按认证方式区别费用显示（订阅不显示费用）。** 费用是按模型价格的估算，与走哪条通道无关。
- **以 pi.dev 的目录作为元数据来源。** 它的字段更贴近需要，但这是另一个产品未公开文档、没有稳定性承诺的接口；models.dev 是公开的开源数据。
- **在配置中描述任意请求改写（模板或 JSON patch）。** 能力没有边界，也无法逐项测试。
- **读取项目目录中的 provider 配置。** 检出的仓库可以借此改变 endpoint。
- **依赖服务端会话状态。** 会与会话树形成两份权威；SIWC 也不允许。
- **推理档位不支持时降级到相近档位。** 会在用户不知情时改变行为。
- **SIWC 作为 `openai` provider 的第二种认证方式。** 同一个 `openai/<model>` 会对应两种凭据与两份模型列表，回放来源也无法区分；单独的 `chatgpt` provider 让每个 provider 只有一种认证方式，endpoint 也能单独固定。
- **提交端读取配置与凭据，经私有 IPC 交给后台（现状）。** 配置是全局状态，提交端读取后再交给后台是多余的一圈；会话的 profile 副本在后台，提交端还要先取回副本；SIWC 的 token 在执行中会过期，而执行时提交端可能已经退出。
- **key 继续由环境变量提供。** 后台常驻，由后台读自己的环境时，key 取决于谁启动了后台，改 key 要重启后台；由提交端随提交附带，又留下一处按提交者变化的来源。
- **前端命令直接写 `auth.json`。** 前端与后台都会写这个文件，要靠文件锁协调；前端还要自己确定配置目录，可能与运行中的后台不一致。
- **接受 Turn 时强制刷新 token。** 每一轮都多一次刷新请求；超过有效期的 Turn 仍要在执行中刷新，请求前的检查省不掉。
- **定时刷新 token。** 机器休眠或进程挂起时定时器会错过，请求前的检查仍然省不掉；后台常驻时，定时刷新让登录永不失效，绕过了 refresh token 30 天不用即失效的设计。
- **保持 3600 秒的 Turn deadline 上限，SIWC 的 Turn 不超过 token 有效期。** 执行中的刷新没有实际用到的场景，只能靠测试证明。
- **凭据存入系统密钥环。** 无头机器通常没有可用的密钥环；本 ADR 采用权限受限的本地文件。
- **每次提交都重读原 profile（现状）。** 已有会话的行为会随 profile 的修改而变化，重新打开会话时无法确定接下来用什么配置。
- **选择后立即写回副本。** 需要新增一条后台命令，且副本可能与最后一轮实际使用的模型不同；随提交写回则两者始终一致。

## 不在本文决定的事项

- 各协议 feature 的完整清单及现有厂商差异的迁移对照，`providers.toml`、`auth.json` 与 SIWC 模型列表的完整格式和保存位置，`lattice auth` 的命令形式，各家 endpoint，用量换算与缓存断点，SIWC 刷新的余量、刷新失败的错误码对照与临时故障的恢复方式：由 `llm.md` 规定。
- 模型候选查询的形状：由 `session.md` 规定。
- 选择面板的具体键位、候选项排序与显示宽度：由 `tui.md` 规定。
- Turn deadline 的粒度、默认值与到点后的结局：由 #222 裁决。
- Vertex AI、Bedrock、Azure 等其他协议或认证方式，以及其他订阅登录。
- 从原 profile 重新生成副本的命令：确实需要时再加。
- 本计划与 #309 的先后。
