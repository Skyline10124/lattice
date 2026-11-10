---
status: active
---

# provider 配置

> 所属领域：[模型接入](README.md)

## 字段

| 字段 | 含义 | 规则 |
| --- | --- | --- |
| `id` | provider 标识，用于模型引用、节点记录、回放来源与凭据查找 | 1～64 个字符，`[a-z0-9]` 开头，只含 `[a-z0-9-]` |
| `protocol` | 上表五个取值之一 | 必填 |
| `base_url` | endpoint | 必填；不含用户名密码、query 与 fragment；非 loopback 地址必须是 `https`；最长 8,192 字节 |
| `auth` | 认证方式 | 默认随协议；`oauth:chatgpt` 只用于内置的 `chatgpt` |
| `features` | 见 [features](protocols.md#features) | 只能取该协议已定义的 feature 与取值 |
| `catalog` | 对应的 models.dev provider 条目 | 可选；只用于取元数据。可以是 models.dev 的任意 provider 条目，不限于内置表中出现的；models.dev 中没有这个条目时，这个 provider 没有元数据，不按错误处理 |
| `models` | 按模型手写的元数据与 features 覆盖 | 可选 |

## 内置 provider 表

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

- `generate-content` 协议保留，需要它的用户在 `providers.toml` 中新增一个使用该协议的 provider；内置 `google` 不能改 `protocol`。OpenAI 的新模型在 Chat Completions 上不能使用工具（[OpenAI 推理指南](https://developers.openai.com/api/docs/guides/reasoning)），所以 `openai` 以 Responses 为准。
- `chatgpt` 是 OpenAI 的订阅通道，限制见 [Sign in with ChatGPT](credentials.md#sign-in-with-chatgpt)。
- 阿里云的文档推荐按工作空间区分的域名（`https://{WorkspaceId}.<地域>.maas.aliyuncs.com/compatible-mode/v1`），表中用的是仍可使用的旧域名；使用工作空间域名的用户在 `providers.toml` 中覆盖 `base_url`。
- `ollama` 在 models.dev 中没有条目，它的模型没有元数据，可以在 `providers.toml` 中手写。

## 用户配置 `providers.toml`

用户全局配置位于配置目录下的 `providers.toml`（默认 `~/.lattice/providers.toml`）。一个 provider 一张表：表名是内置 `id` 时覆盖该 provider 的字段，否则新增一个 provider。

```toml
[metadata]
auto_refresh = false                   # 关闭自动刷新，默认 true

[providers.my-vllm]                    # 新增自建服务
protocol = "chat-completions"
base_url = "https://vllm.example.com/v1"

[providers.my-vllm.features]
thinking_toggle = "enable-thinking"    # 无元数据来源时，off/high 写关闭/开启开关

[providers.my-vllm.models."qwen3-32b"] # 元数据中没有的模型，手写元数据
context = 131072
output = 8192
reasoning = true
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
| `auto_refresh` | 顶层 `metadata` 表 | 布尔，默认 true。每次自动触发都读取当前文件：false 时跳过本次拉取；文件无效时本次自动刷新算作失败，按拉取失败的退避重试。`lattice models refresh` 不受影响 |
| `protocol`、`base_url`、`auth`、`catalog` | provider 表 | 见[字段](#字段)；覆盖内置 provider 时只写要改的字段，不能写 `protocol` |
| `features` | provider 子表 | 覆盖或补充 provider 级 features |
| `models."<模型>"` | provider 子表 | 手写元数据：`context`、`output`（token 数），`reasoning`（布尔，覆盖目录元数据中的同名标志，缺失时继承），`effort`（可用档位列表；无元数据来源时[按 features 推出写法](reasoning.md#可用档位)），`tools`（布尔，默认 true），`cost`（`input`、`output`、`cache_read`、`cache_write`，美元每百万 token） |
| `models."<模型>".features` | 模型子表 | 按模型覆盖 features |

没有目录元数据的模型，在 Responses、Interactions、generateContent 或 Messages 上请求推理文字时需要手写 `reasoning = true`；Responses 也据此请求 include 中的加密 reasoning。该标志不增加或移除可选档位，档位仍按[可用档位](reasoning.md#可用档位)的规则推导。

- **严格解析。** 未知字段、未知 feature 或取值、类型不符、格式错误都使整个文件无效。文件无效时，用到 provider 配置的操作在接受前按配置错误拒绝，诊断写明文件路径、行号与原因；不使用部分有效的内容。
- **文件不存在**等于空配置，只使用内置表。
- **免凭据内置项的声明要求。** 内置表中 `auth = "none"` 的免凭据 provider 须在 `providers.toml` 中出现对应的 `[providers.<id>]` 表才视为已声明并可使用，仅写 `models` 等子表形成的隐式父表同样有效，详情见[可用性判定](models.md#可用性判定)。
- **内置 provider 不能改协议。** 覆盖内置 provider 的表写了 `protocol` 时，即使取值与内置相同，整个文件也无效；要用其他协议，新建一个 provider `id`。
- **`chatgpt` 不能改写。** 文件中出现 `[providers.chatgpt]` 的 `protocol`、`base_url`、`auth` 或 `features`，或其他 provider 设置 `auth = "oauth:chatgpt"`，整个文件无效。`chatgpt` 只能写 `models` 下的手写元数据。
- **不写凭据。** 文件中没有 key 字段；凭据只在 `auth.json` 中。
- **生效时机。** 请求网关每次请求、后台接受 Turn 与列出模型候选时，都按文件的当前内容解析。用于请求发送的配置修改从下一次请求起对所有会话生效；执行中的一轮可能前后两次请求发往不同的 endpoint。价格的生效时机见[费用估算](usage.md#费用估算)。文件在执行中变为无效时，下一次发送请求按 `ProviderError { retryable: false }` 停止，诊断写明是配置问题，见[会话合同](../session/acceptance.md#模型请求与执行预算)。

## endpoint 的来源

key 会发到 endpoint，所以 endpoint 只来自内置表与用户全局的 `providers.toml`：

- 项目目录中的任何文件都不提供 provider 配置。agent profile 可以放在项目内，它只能写模型引用，不能改变 endpoint 或认证方式。
- 环境变量与工作区 `.env` 不提供 key、endpoint 或模型。
- 模型元数据只有[元数据的来源与合并](models.md#元数据的来源与合并)中列出的字段，不含 endpoint。

地址检查与连接时的地址固定沿用[会话合同的模型 HTTP 传输](../session/concurrency.md#输入与传输限制)与 `net-guard` 的规则：localhost、127.0.0.1、::1 是 loopback，其他私有与保留地址拒绝。
