---
status: active
---

# 协议适配器与 features

> 所属领域：[模型接入](README.md)

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
| `oauth:chatgpt` | `Authorization: Bearer <access token>`，见 [Sign in with ChatGPT](credentials.md#sign-in-with-chatgpt) |
| `none` | 不带认证头，用于本地服务 |

请求头只有认证头、协议要求的头（`anthropic-version`）与[会话合同](../session/concurrency.md#输入与传输限制)列出的通用头；认证头与请求路径由 `protocol` 与 `auth` 决定。

### 适配器的职责

- **请求。** 把当前路径上的对话、系统提示、工具定义和推理档位按协议与 features 写成请求体；工具名原样发送，generateContent 的工具 schema 放在 `parametersJsonSchema`，Chat Completions 总是发送 `stream_options: {"include_usage": true}`。不发送采样参数（`temperature`、`top_p` 等），由服务端按模型默认值处理。投影规则与回放材料的处理由[会话合同的回放与跨 provider 投影](../session/conversation.md#回放与跨-provider-投影)维护。
- **流式事件。** 把线上事件翻译为正文、reasoning 与工具调用的部件增量，交给 #312 的累积器组装完整回复。解析端对无歧义的厂商差异直接兼容，例如 `reasoning_content` 与 `reasoning` 两种字段都读取，Mistral 的 `content` 为分块数组时读取其中的 `thinking` 块。不认识的内容块或增量类型按不可重试的协议错误停止这次尝试，不静默丢弃；诊断只写协议、位置与类型名。
- **推理文字。** 仅在模型元数据明确 `reasoning = true` 时请求：Interactions 写 `generation_config.thinking_summaries: "auto"`；generateContent 在推理未关闭时写 `generationConfig.thinkingConfig.includeThoughts: true`，`thinkingBudget: 0` 和 toggle 关闭均不写。这两项与是否选档位无关，分别与现有 generation_config、thinkingConfig 字段合并。Messages 在同一元数据条件且 thinking 为 adaptive 时写 `display: "summarized"`，budget 和 disabled 不写；不选档位时不发 `thinking`，具体写法与原因见[档位映射](reasoning.md#档位映射)。`false` 或没有该标志时均不请求推理文字，所选档位照常发送。
- **Responses 的不完整回复。** `response.incomplete` 先更新用量；原因是 `max_output_tokens` 或 `content_filter` 时按完成处理，分别记 `finish_reason = length` 或 `content_filter`，关闭仍打开的 item 并保存回复，允许缺少 `output_item.done`。半截工具参数仍由累积器的批次校验拒绝，不执行工具；完整调用沿用 Chat Completions 在 `length` 下的处理。其他或未知原因按 provider 错误停止，不保存回复。
- **Interactions 的不完整回复。** `interaction.completed` 的 `status = incomplete` 按完成处理，先更新用量，关闭仍打开的 step 并保存回复，记 `finish_reason = incomplete`。协议没有给出具体原因，不将它推断为 `length`；半截工具参数仍由累积器的批次校验拒绝，不执行工具。`failed`、`cancelled`、`requires_continuation` 继续按 provider 错误停止，不保存回复。
- **prompt 拦截。** generateContent 只有 `promptFeedback.blockReason`、没有 candidates 时，按不可重试的 provider 错误停止这次尝试，诊断保留 `blockReason` 原文。
- **用量。** 按[用量换算](usage.md#用量换算)得出统一口径。
- **不使用服务端会话状态。** Responses 与 Interactions 固定发送 `store: false`，每次发送完整历史，不发送 `previous_response_id`、`previous_interaction_id`，不使用后台执行。

只有 Anthropic Messages 要求发送输出上限：`max_tokens` 直接取模型元数据的输出上限，缺失或为 0 时使用 32,768。其他协议不发送输出上限（例如 Responses 的 `max_output_tokens`），由服务端按模型默认值处理。

服务端对提示与 `max_tokens` 之和超出上下文窗口的处理依模型而异：Claude 4.5 以前返回 HTTP 400；第三方兼容端超窗时也可能直接返回 HTTP 400；Claude 4.5 及以后在提示本身可容纳时可接受请求，生成到窗口边界后以 `stop_reason = "model_context_window_exceeded"` 结束。该停止原因映射为 `finish_reason = "context_window_exceeded"`，与 `length` 区分：`length` 是输出达到 `max_tokens`，继续生成或调高输出上限即可；`context_window_exceeded` 是上下文窗口已满，继续只会更长，需要压缩历史或开新会话。

### 回放来源

每次尝试记录来源 `(protocol, provider id)`。reasoning、签名、加密 reasoning 与逐片段回放状态只在协议和 provider `id` 都相同时回放；任一不同时去除这些材料。来源不含 endpoint：同一 provider `id` 在 `providers.toml` 中改了 `base_url` 之后仍回放此前的签名，新 endpoint 可能拒绝这些签名；要换 endpoint 时使用新的 provider `id`。

provider call ID 在工具调用与工具结果两侧原样保留，切换协议或 provider 都不去除。对话历史中的工具调用缺少 call ID 时省略对应字段，不合成 ID，也不在本地判断目标协议是否接受，由目标 API 返回错误。所有协议的服务端工具调用响应都必须带 call ID；Chat Completions 流式调用以首次出现的增量为准，后续增量可省略 ID。缺少 ID 时当轮以不可重试的 malformed 错误停止，不保存回复、不执行工具。跨来源投影保留中立语义，切回原来源时使用树中保存的原消息。

Responses 仅在模型元数据明确 `reasoning = true` 时带 `include: ["reasoning.encrypted_content"]`，使 `store: false` 时取得可回放的加密 reasoning；`false` 或没有该标志时不发送，是否选择档位及档位为 `off` 都不改变 include 的条件。Responses 回放时省略没有最终 `encrypted_content` 的 reasoning item，以及末尾没有后续 item 的 reasoning item；其文字仍以 Reasoning 语义保存。

Anthropic Messages 的 `redacted_thinking` 以空 Reasoning 片段及带 `redacted:` 标记的回放状态保存 `data`，只有 Messages codec 解释该标记；普通 thinking（包括空文本）的 signature 保持原编码。同完整来源时恢复原块、data 与顺序，跨来源时移除整个 Reasoning 片段，不产生空 Text。标记计入单片段 64 KiB 与尝试总量限制，data 不进入正文或 reasoning 显示增量。

## features

feature 由协议适配器的代码定义，配置只能在已有取值中选择。下表是已定义的全部 feature；新的厂商差异先在适配器中增加取值并测试，再在内置表或 `providers.toml` 中启用。

### Chat Completions

| feature | 取值 | 默认 | 作用 |
| --- | --- | --- | --- |
| `effort_param` | `reasoning-effort`、`reasoning-object` | `reasoning-effort` | 推理档位写作顶层 `reasoning_effort: "<档位>"`，或 OpenRouter 的 `reasoning: {"effort": "<档位>"}`；`off` 写作 `"none"` |
| `thinking_toggle` | `none`、`thinking-type`、`enable-thinking` | `none` | 关闭与开启推理的写法：`none` 时来自 toggle 项的 `off` 与 `high` 不可用；`thinking-type` 写 `thinking: {"type": "enabled" \| "disabled"}`；`enable-thinking` 写 `enable_thinking: true \| false` |
| `thinking_budget` | `none`、`thinking-budget`、`reasoning-max-tokens` | `none` | 按 token 数限制推理的写法：顶层 `thinking_budget: N`，或 OpenRouter 的 `reasoning: {"max_tokens": N}` |
| `reasoning_replay` | `none`、`reasoning-content`、`content-chunks` | `none` | 同来源回放时，助手消息中的 reasoning 怎样发回：不发；写作 `reasoning_content` 字段（内置表中需要它的厂商启用 `reasoning-content`）；写作 Mistral 的 `content` 分块（`thinking` 块在前） |
| `cache_markers` | 布尔 | `false` | 在系统提示与最后一条消息的末尾内容块上加 `cache_control: {"type": "ephemeral"}`，用于阿里云的显式缓存 |

推理参数各部分的组合见[档位映射](reasoning.md#档位映射)。

### Responses

Responses 没有 feature。推理摘要请求由模型元数据决定，见[档位映射](reasoning.md#档位映射)。

适配器只发送 SIWC 也接受的字段（见[适配器的职责](#适配器的职责)），所以 `chatgpt` 不需要额外的 feature。

SIWC 的 `response.completed` 终态可能携带 `output: []`；此时解码器使用流中已通过 `response.output_item.done` 完成的 item，任一 item 未结束仍报错。非空 `output` 继续与流中的 item 交叉校验，缺少 `output` 仍报错。`response.incomplete` 的空 `output` 按[不完整回复](#适配器的职责)规则关闭仍打开的 item。

### Anthropic Messages

| feature | 取值 | 默认 | 作用 |
| --- | --- | --- | --- |
| `cache` | `automatic`、`none` | `automatic` | `automatic`：系统提示最后一块加显式断点，请求顶层加 `cache_control: {"type": "ephemeral"}` 由服务端放置对话末尾的断点；`none`：不发送缓存字段 |

`cache` 只在 Messages 上合法，支持 provider 级配置与 model 级覆盖。adaptive 的推理文字请求由模型元数据决定，见[档位映射](reasoning.md#档位映射)。

### Google generateContent 与 Interactions

这两种协议没有 feature。
