---
status: proposed
---

# ADR 0010：模型接入按协议适配，provider 改为配置

- 状态：Proposed（草案，待维护者批准）
- 日期：2026-10-24
- 修订：细化 ADR 0005 第 7 节“同 provider 回放”中来源的粒度；部分取代 ADR 0006 第 4 节中“当前 agent 即当前位置所属 Turn 的 agent”的规则。范围见[与 ADR 0005、ADR 0006、ADR 0007 的关系](#与-adr-0005adr-0006adr-0007-的关系)
- 依赖：ADR 0007（接受时确定的模型与 provider 记在 UserMessage 节点上）
- 实施跟踪：GitHub #315

## 背景

`lattice-llm` 把模型请求发给 provider，并把流式响应交给回复组装。以下按 `main` 的 `a90e4c0a` 核对。

- **适配按厂商划分。** `ProviderKind` 有 `OpenAi`、`OpenAiCompatible`、`Anthropic`、`Gemini` 四个值（`crates/lattice-llm/src/router.rs`）。`client.rs` 按它决定认证头、请求路径和请求构造，`connection.rs`、`lattice-api` 的冻结配置也带着它。同一协议的不同厂商只能共用一个 `OpenAiCompatible`。
- **模型靠写死的别名表选择。** 可选的只有 `gpt-5.5`、`gpt-5.4`、`gpt-5.3-codex`、`gpt-4o-mini`、`opus`、`sonnet`、`haiku`、`gemini`，以及 `compat:<模型>`。新模型上线或旧模型下线都要改代码：#269 中 `gemini` 指向的模型对新用户已不可用；`gpt-5.3-codex` 只能走 Responses API，按现有路径请求返回 404（#271 的评论）。
- **会话中不能切换模型。** 模型只在 profile 中设定，CLI 的 `session send` 只能换 agent，TUI 没有切换模型或推理强度的入口。
- **每次提交都重读 profile。** 提交端在每次提交前从磁盘读取所选 agent 的 `agent.toml` 并解析外部策略文件，冻结进这一轮；会话本身不保存配置，当前 agent 是当前位置所属 Turn 的 agent。改动 `agent.toml` 会改变已有会话下一轮的行为。
- **endpoint 与凭据每家固定一套。** 每个 provider 一个固定的 key 环境变量；OpenAI 兼容只能配置一个 endpoint；官方 OpenAI 的 endpoint 不能改写。
- **厂商差异散在代码里。** 例如 Chat Completions 解析同时读取 `reasoning_content` 与 `reasoning` 两个字段；Gemini 的工具 schema 改用 `parametersJsonSchema`（#270）。这些差异不能按 provider 配置，新增一家有差异的兼容厂商就要加分支。
- **没有模型元数据。** 不知道上下文窗口、输出上限和推理能力。Anthropic 的 `max_tokens` 用固定默认值；请求从不带推理参数，`gpt-6-luna` 在 Chat Completions 上不关闭推理就不能使用工具（#226 的评论）。
- **用量只有三项。** `TokenUsage` 只有 `prompt_tokens`、`completion_tokens`、`total_tokens`，按尝试保存在会话库中；推理与缓存 token 不区分，请求也不开启缓存。
- **回放来源按协议记录。** `ProviderOrigin` 只有 `OpenAiCompatible`、`Anthropic`、`Gemini`，所有 OpenAI 兼容厂商共用一个来源。
- **凭据只在内存中流转。** key 由提交端读取，冻结进本 Turn 的连接材料，经私有 IPC 交给后台，不持久化（`docs/architecture/session.md` 的配置安全边界）。

外部条件也在变化：

- OpenAI 的新模型和部分能力只在 Responses API 上提供。
- Google 的 Interactions API 于 2026 年 6 月 GA，是 Google 推荐的接口，新模型与新功能先在这里上线；`generateContent` 仍完整支持。
- OpenAI 于 2026-09-29 推出 Sign in with ChatGPT（SIWC），开源与本地运行的工具可以让用户用 ChatGPT 订阅额度调用 Responses API。它要求客户端持久保存登录材料。

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

### 2. provider 是配置

provider 由以下字段描述，不对应单独的代码：

| 字段 | 含义 |
| --- | --- |
| `id` | provider 标识，用于模型引用、节点记录与回放来源 |
| `protocol` | 第 1 节的五个协议之一 |
| `base_url` | endpoint |
| `auth` | 认证方式：`bearer`、`x-api-key`、`x-goog-api-key`、`oauth:chatgpt` 或 `none`（本地服务），默认值随协议 |
| `api_key_env` | key 所在的环境变量名；`oauth:*` 与 `none` 认证不使用 |
| `features` | 第 3 节 |
| `catalog` | 对应的 models.dev provider 条目，用于取模型元数据 |

官方厂商与兼容厂商用同一种描述。例如 DeepSeek 是 `protocol = "chat-completions"`、`base_url = "https://api.deepseek.com"`、`api_key_env = "DEEPSEEK_API_KEY"`，再加上它需要的 features。

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

### 4. endpoint 与凭据的来源

key 会发到 endpoint，所以 endpoint 只来自经过审查或用户自己写的配置。

- **内置 provider 表。** 手工维护，提交进仓库，编译进程序。每一家的协议、endpoint、认证方式、key 环境变量名与 features 对照厂商文档核对，变化在 PR 中审查；models.dev 只通过 `catalog` 字段提供模型元数据。第一轮收录 20 家常用 provider：

  | 分组 | provider |
  | --- | --- |
  | 官方 | `openai`、`anthropic`、`google`、`xai`、`mistral` |
  | 国内 | `deepseek`、`moonshotai`、`moonshotai-cn`、`zai`、`zhipuai`、`alibaba`、`alibaba-cn`、`minimax`、`minimax-cn`、`volcengine`、`siliconflow-cn` |
  | 聚合 | `openrouter`、`groq` |
  | 本地 | `ollama`、`lmstudio`（`auth = "none"`） |

  国际站与国内站 endpoint 不同的厂商各算一家。表中不记录某一家是否经过实测；实际验证范围写在实现 PR 的验证记录中。
- **用户全局 `~/.lattice/providers.toml`。** 新增 provider，或覆盖内置 provider 的字段。一个 provider 一张表，`models` 子表按模型补充元数据，`features` 子表设置厂商差异，例如：

  ```toml
  [providers.my-vllm]                   # 新增自建服务
  protocol = "chat-completions"
  base_url = "https://vllm.example.com/v1"
  api_key_env = "MY_VLLM_KEY"

  [providers.my-vllm.models."qwen3-32b"] # 元数据中没有的模型，手写元数据
  context = 131072
  output = 8192

  [providers.deepseek]                  # 覆盖内置 provider 的 endpoint
  base_url = "https://gateway.example.com/deepseek"

  [providers.deepseek.features]
  thinking_format = "thinking.type"
  ```

  完整字段由合同规定。
- **不读项目目录。** 项目目录随仓库检出，其中的配置不可信，不能改变 endpoint 或认证方式。这与工作区 `.env` 不能设置 endpoint（#99）是同一条边界；工作区 `.env` 仍可按现有规则提供 key。
- **配置中不写 key 明文。** 只写环境变量名；订阅登录的材料见第 11 节。
- **环境变量只提供 key。** endpoint 与模型不再从环境变量读取：`OPENAI_COMPATIBLE_BASE_URL`、`OPENAI_COMPATIBLE_MODEL`、`OPENAI_COMPATIBLE_API_KEY`、`ANTHROPIC_BASE_URL`、`GEMINI_BASE_URL` 删除，内置的 `openai-compatible` provider 与 `compat:<模型>` 写法一并删除。本地服务使用内置的 `ollama`、`lmstudio`，或在 `providers.toml` 中配置。每个 provider 只有一个 key 环境变量名。
- **非 loopback 的 endpoint 必须使用 HTTPS。** 沿用现有规则，对内置表与 `providers.toml` 同样适用。
- **元数据不提供 endpoint。** 第 6 节的元数据（内置快照与缓存）中即使有 endpoint、环境变量名或协议字段也不使用。

provider 配置在提交端解析，写入会话的 profile 副本（第 13 节）；每一轮从副本冻结进连接材料。

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
- **请求路径上不联网。** 提交 Turn 时只读上述数据；本 Turn 用到的值（如输出上限、可用推理档位）随连接材料冻结。
- **没有元数据的模型仍可使用。** 只是没有价格与推理档位。

### 7. 推理强度

- 统一档位：`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`，`max` 在 `xhigh` 之上。
- 模型可用的档位由元数据给出（models.dev 的 `reasoning_options`），协议适配器把档位映射为该协议的参数。
- 选了模型不支持的档位时，在接受前按配置错误拒绝，不降级到相近档位。
- profile 的 `[agent].effort` 设默认档位，会话中可以切换（第 13 节）。不设档位时不发送推理参数，由 provider 使用默认行为。生效的档位随 Turn 冻结并记在 UserMessage 节点上。

### 8. 用量细分、费用与缓存

- 每次尝试的用量细分为输入、缓存读、缓存写、输出与其中的推理。各协议的报告口径不同（例如 OpenAI 的输入含缓存命中，Anthropic 不含），由协议适配器换算为统一口径，换算规则写入合同。
- provider 没有报告的细分项为未知，不记为 0，与现有“缺失的用量为未知”一致。
- 只保存 token 数。费用在显示时按模型的当前价格计算，标为估算；没有价格时不显示费用。费用只按模型价格计算，不因 provider 或认证方式区别，SIWC 同样显示。
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

SIWC 是一种认证方式：`protocol = "responses"`、`auth = "oauth:chatgpt"`，加上 SIWC 要求的 features（固定 `store: false`，不发送 `temperature`、`max_output_tokens` 等不支持的参数）。它不需要单独的协议适配器。

- **登录。** 新增登录命令，按 OpenAI 开源工具接入方式完成授权码加 PKCE 流程，回调地址为 `http://127.0.0.1:1455`；无头机器可以粘贴浏览器最终跳转的 URL。
- **登录材料落盘。** `client_id`、host ID 与 access、refresh、id token 保存在 `~/.lattice/auth.json`，文件权限 0600。不写入会话库、日志、诊断或公开输出。
- **刷新在提交端。** 提交端在冻结配置前检查 access token；剩余有效期短于 Turn deadline 加余量时先刷新。刷新用文件锁串行化，同一份登录材料同一时间只有一个进程在刷新。
- **后台不接触登录材料。** 冻结的 access token 与现有 API key 一样经私有 IPC 交给后台，只在内存中使用。
- **登出。** 撤销 refresh token 后删除本地材料；撤销未确认时仍删除本地材料，并提示用户在 ChatGPT 设置中断开应用。
- **模型列表。** SIWC 账号可用的模型以 `GET /v1/models` 的结果为准，在登录与 `lattice models refresh` 时由提交端拉取；推理档位与价格仍取第 6 节的元数据。
- **用量超限。** SIWC 的用量超限（例如 `subscription_sharing_usage_limit_exceeded`，可能在流中途出现）按不可重试的 provider 错误处理。

这改变了“凭据不落盘”的现行边界：落盘的只有 SIWC 登录材料，API key 仍只从环境变量读取。

### 12. Google 内置 provider 的默认协议

Interactions 适配器上线后，内置 `google` provider 默认使用 Interactions；`generateContent` 保留，可在 `providers.toml` 中切换。

### 13. 会话持有 profile 副本

会话建立时取一份 profile 副本，此后只用副本，不再读取原 profile。

- **创建。** 新会话首次提交时，提交端读取所选 agent 的 profile（解析外部策略文件、加载 system prompt）和所选模型的 provider 配置，随提交交给后台；后台在接受事务中把它写成这个会话的 profile 副本。
- **内容。** 有效 profile、provider 配置（协议、endpoint、认证方式、features、key 环境变量名）、当前模型与推理档位。不含 key 与订阅登录材料，副本可以持久化而不改变凭据边界。
- **使用。** 已有会话的后续提交只用副本，提交端不再读取原 `agent.toml`。提交端从会话头取得副本中的 provider 与认证方式，据此读取 key 或刷新 SIWC token，随提交交给后台。
- **修改。** `/model`、`/effort` 与 CLI 的 `--model`、`--effort` 选择的值随下一次提交交给后台，在接受事务中写回副本。换到另一个 provider 的模型时，提交端按当时的内置表与 `providers.toml` 解析该 provider 的配置，一并写入。`--agent` 换 agent 时，用该 agent 的 profile 重新生成副本。
- **选择随提交生效。** 选择后未发送就退出，这次选择不保留。所以副本中的模型与档位总是最后一轮实际使用的，重新打开会话时沿用最后一轮的模型与档位。
- **rewind 不改变副本。** 副本属于会话，不属于节点；当前 agent 是副本中的 agent。每一轮实际使用的 agent、模型、provider 与档位仍记在它的 UserMessage 节点上。
- **原 profile 的修改只对新会话生效。** 包括收紧的 LANCET 策略（写权限、网络白名单）；要让已有会话使用新策略，需要开新会话。

### 14. TUI 的 `/model` 与 `/effort`

TUI 用 slash 命令选择模型与推理强度，选择界面是 Dock 中的面板。

- **面板可以交互。** 现有面板只显示 `/help` 的输出；选择面板在 Dock 中列出候选项，输入文字过滤，↑↓ 移动，Enter 选定，Esc 取消。焦点在面板时由面板解释按键。
- **`/model`。** 列出可用的 `<provider>/<model>`：provider 来自内置表与 `providers.toml`，模型来自元数据缓存与 `providers.toml` 中手写的模型。候选项标明凭据是否就绪。
- **`/effort`。** 只列出当前所选模型支持的档位。模型没有元数据时，面板说明该模型不能选择推理强度，可以在 `providers.toml` 中为它手写档位。
- **切换模型时。** 当前档位新模型不支持时，改回副本中 profile 的默认档位；默认档位也不支持时清除，由状态消息说明。
- **作用范围。** 选择由 TUI 进程持有，随下一次提交写回会话的 profile 副本，不写回原 profile（第 13 节）。
- **statusline。** 显示下一次提交将使用的模型、provider 与档位：有未提交的选择时显示所选值，否则显示副本中的值。

候选列表与 provider 配置、元数据缓存一样由 TUI 所在的提交端进程读取，不新增后台接口。

### 15. 合同先行

实施前先完成以下合同修订并获批准：

- 新增 `docs/architecture/llm.md`，作为协议、provider 配置、features 清单、元数据、推理档位映射、用量换算与订阅登录规则的唯一出处。
- `docs/architecture/session.md`：会话的 profile 副本、当前 agent 的判定与提交流程（第 13 节），会话头新增的副本摘要，冻结配置表（provider、模型、推理档位、连接材料），配置安全边界（第 4、11 节），回放与跨 provider 投影（第 10 节），用量记录（第 8 节）。
- `docs/architecture/tui.md`：打开已有会话不再加载 profile，可交互的面板、`/model` 与 `/effort`、statusline 字段（第 13、14 节）。
- `docs/development/testing.md` 与 `crates/lattice-system-harness/AGENTS.md`：fake provider 改为经测试目录中的 `providers.toml` 接入，不再经环境变量。

### 16. 旧数据

- 旧会话可以继续读取与续接。节点上旧的 provider 值映射到内置 provider：`openai`、`anthropic` 保持不变，`gemini` 映射为 `google`；`openai_compatible` 无法确定是哪一家，按未知来源处理，续接时按跨来源规则投影。
- 旧的用量记录中 `prompt_tokens`、`completion_tokens` 读作输入与输出，缓存与推理细分为未知。OpenAI 兼容的旧记录中输入含缓存命中，已无法拆分。
- 旧会话没有 profile 副本。升级后第一次提交时，按当前位置所属 Turn 的 agent 读取 `agent.toml`，生成副本。
- 新版本写入的记录，旧版本无法读取。

### 17. 验证方式

- 每个协议适配器有基于官方文档的请求体与流式事件夹具，覆盖文字、推理、多个工具调用、用量与错误事件。
- 每个 feature 取值有请求体断言，默认值与标准协议形状一致。
- 元数据的取值优先级与刷新：手写值、缓存与内置快照的覆盖顺序；后台刷新失败时继续使用上一份数据。
- endpoint 来源的负向测试：项目目录中的 `providers.toml` 不被读取，元数据缓存中的 endpoint 不被使用，环境变量与工作区 `.env` 都不能改变 endpoint。
- TUI 的选择面板由 pty 场景覆盖选择、过滤与取消；选择后提交的请求体带上所选模型与档位。
- 已有会话在原 `agent.toml` 修改或删除后仍按副本执行；切换模型后重新打开会话，沿用最后一轮的模型与档位；rewind 不改变副本。
- 有凭据时用真实 provider 验证：OpenAI API key（Chat Completions、Responses、推理、缓存），ChatGPT 订阅账号（SIWC），Gemini API key（generateContent、Interactions）。没有凭据的 provider 只按文档与夹具验证，并在交付时说明。

## 与 ADR 0005、ADR 0006、ADR 0007 的关系

- **ADR 0005 第 7 节。** 同 provider 回放可以使用不透明回放材料，跨 provider 投影只使用中立语义。本 ADR 保留这条规则，把其中的“provider”细化为“协议 + provider `id`”（第 10 节）。
- **ADR 0006 第 4 节。** “不设 Session 级当前 agent 字段，当前 agent 即当前位置所属 Turn 的 agent，rewind 后回到当时的 agent”由第 13 节取代：当前 agent 是会话 profile 副本中的 agent，rewind 不改变它。`send --agent` 仍可换 agent，改为重新生成副本。
- **ADR 0007 第 6 节。** 接受时确定的事实仍记在 UserMessage 节点上，配置表不变。该节删除的是每一轮各存一份的 profile 快照与策略快照，理由是它们没有读者、内容与节点重复。本 ADR 的副本每个会话一份，是后续提交的配置来源，两者不冲突。

三份 ADR 的其余内容不受影响。

## 后果

**收益**

- 接入一家使用已有协议的厂商只需写配置；常见厂商由内置表直接提供，用户只设 key 环境变量。
- 厂商差异集中在协议适配器的 features 中，每项有名字和测试，不再散在代码分支里。
- 模型随 provider 上线即可使用，不再依赖别名表更新。
- 推理强度、用量细分、缓存与费用估算有了统一的来源与口径。
- Responses、Interactions 与 SIWC 共用同一套 provider 与组装机制。

**代价**

- 破坏性变化：现有 profile 中的别名失效，须改为 `<provider>/<model>`；用环境变量配置 endpoint 或 OpenAI 兼容服务的用户须改写为 `providers.toml`；会话库记录格式变化，旧版本读不了新库。
- harness 的 fake provider 目前经环境变量接入，要随之迁移。
- 内置表需要手工维护；厂商改了 endpoint 或协议细节，要发新版本更新。内置表之外的 provider 由用户在 `providers.toml` 中自行添加。
- 后台每天访问一次 models.dev。
- feature 集合会随接入的厂商增长。
- SIWC 让登录材料落盘，增加了一类需要保护的本地文件。
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
- **登录材料存入系统密钥环。** 无头机器通常没有可用的密钥环；本 ADR 采用权限受限的本地文件。
- **每次提交都重读原 profile（现状）。** 已有会话的行为会随 profile 的修改而变化，重新打开会话时无法确定接下来用什么配置。
- **选择后立即写回副本。** 需要新增一条后台命令，且副本可能与最后一轮实际使用的模型不同；随提交写回则两者始终一致。

## 不在本文决定的事项

- 各协议 feature 的完整清单及现有厂商差异的迁移对照，`providers.toml` 与 `auth.json` 的完整格式，各家 endpoint 与 key 环境变量名，用量换算与缓存断点：由 `llm.md` 规定。
- 选择面板的具体键位、候选项排序与显示宽度：由 `tui.md` 规定。
- Vertex AI、Bedrock、Azure 等其他协议或认证方式，以及其他订阅登录。
- 从原 profile 重新生成副本的命令：确实需要时再加。
- 本计划与 #309 的先后。
