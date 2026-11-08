---
status: active
---

# 用量、缓存与费用

> 所属领域：[模型接入](README.md)

## 用量换算

每次尝试的用量按统一口径保存，各项都可以缺失；provider 没有报告的项为未报告，不记为 0。generateContent 在完成时有补零规则，见表后说明；未报告项是否使尝试的用量不全，由[用量与价格表的比对](#用量与价格表的比对)决定：

| 项 | 含义 |
| --- | --- |
| `input` | 全部输入 token，含缓存读与缓存写 |
| `cache_read` | `input` 中从缓存读取的部分 |
| `cache_write` | `input` 中写入缓存的部分 |
| `output` | 全部输出 token，含推理 |
| `reasoning` | `output` 中的推理部分 |

| 协议 | `input` | `cache_read` | `cache_write` | `output` | `reasoning` |
| --- | --- | --- | --- | --- | --- |
| Chat Completions | `prompt_tokens` | `prompt_tokens_details.cached_tokens`；DeepSeek 为 `prompt_cache_hit_tokens` | `prompt_tokens_details.cache_creation_input_tokens`；OpenAI 原生与 OpenRouter 的 `prompt_tokens_details.cache_write_tokens` 也接收，两者都有时取前者 | `completion_tokens` | `completion_tokens_details.reasoning_tokens` |
| Responses | `input_tokens` | `input_tokens_details.cached_tokens` | `input_tokens_details.cache_write_tokens`（含在 `input_tokens` 中，`cached_tokens` 不含写入） | `output_tokens` | `output_tokens_details.reasoning_tokens` |
| Anthropic Messages | `input_tokens` + `cache_creation_input_tokens` + `cache_read_input_tokens` | `cache_read_input_tokens` | `cache_creation_input_tokens` | `output_tokens` | — |
| generateContent | `promptTokenCount` + `toolUsePromptTokenCount` | `cachedContentTokenCount` | — | `candidatesTokenCount` + `thoughtsTokenCount` | `thoughtsTokenCount` |
| Interactions | `total_input_tokens` + `total_tool_use_tokens` | `total_cached_tokens` | — | `total_output_tokens` + `total_thought_tokens` | `total_thought_tokens` |

表中为 — 的项表示该协议从不报告；对参与计价的四项，模型价格表含这一项时，该协议上的尝试为 partial，费用在有可计价项时为估算，否则为未知。

- 计数保存为逐项可选的 `u64`；字段为非 null 即为已报告，明确的 0 也算；缺失或 null 为未报告。全部未报告时整份用量为未知，generateContent 完成时按下述例外处理。字段类型或数值范围错误仍为 `MalformedUsage`。
- Chat Completions 两种缓存读字段同时存在时，优先 `prompt_tokens_details.cached_tokens`，否则取 `prompt_cache_hit_tokens`；缓存写优先 `prompt_tokens_details.cache_creation_input_tokens`，否则取 `prompt_tokens_details.cache_write_tokens`。优先字段明确为 0 时也不取别名，各项只计一次。输入与缓存分项、输出与推理分项之间不校验大小关系，不因 provider 报告不自洽而拒绝或丢弃计数。
- 流中保存本次尝试的原生累计分项，再按表换算。后续事件缺失或 null 的字段保留此前已知值，明确的 0 可以覆盖；重复累计报告不相加。Messages 的 `message_start` 与 `message_delta` 对三个输入字段 `input_tokens`、`cache_read_input_tokens`、`cache_creation_input_tokens` 使用同一例外：已有值时收到 0 保留旧值，旧值缺失时收到 0 则保存 0，正数覆盖旧值；`output_tokens` 仍按最新值覆盖。后续解析失败或尝试停止时仍交付此前已知的快照。
- Messages input 的基础项是 `input_tokens`，generateContent input 与 output 的基础项分别是 `promptTokenCount`、`candidatesTokenCount`。基础项缺失时和为未知；基础项已知时，加上其余已报告的分项。
- generateContent 完成时，若本次尝试的 `usageMetadata` 至少有一个非 null 字段，则在累积快照上把缺失的 `candidatesTokenCount`、`cachedContentTokenCount`、`thoughtsTokenCount`、`toolUsePromptTokenCount` 按 0 处理；`promptTokenCount` 缺失仍使 `input` 为未知，`output` 为 candidates 与 thoughts 之和，`cache_write` 始终未报告。流式只在候选的 `finishReason` 分支执行这次最终映射，完整 wire 入口使用同一映射；逐块更新和无 `finishReason` 的中断不补零。没有收到过含非 null 字段的 `usageMetadata`（缺失、null、`{}` 或仅含 null）时整份用量仍为未知。
- Interactions 的 `total_output_tokens` 是否含推理，Google 文档没有写明；表中按不含处理，实现时用真实响应核对，结论写回本表。

## 缓存的开启与检测

| 协议 | 开启 | 检测 |
| --- | --- | --- |
| Chat Completions | 服务端自动缓存；`cache_markers` 为 true 时加显式断点 | `cache_read`、`cache_write` |
| Responses | 服务端自动缓存；请求带 `prompt_cache_key`，取 SessionId | `cache_read`、`cache_write` |
| Anthropic Messages | 按 `cache` feature | `cache_read`、`cache_write` |
| generateContent、Interactions | 服务端隐式缓存，请求不加字段 | `cache_read` |

缓存键与断点只用 SessionId 和请求结构，不含用户内容。显式断点最多 4 个；Anthropic Messages 的系统提示断点与顶层自动断点合计 2 个，`cache_markers` 合计 2 个。

缓存字段只在历史投影所得的请求副本上添加，不写回会话消息或回放状态。Chat Completions 只在系统提示与最后一条消息各自已有的合法末尾内容块上标记；需要承载断点的非空文本字符串转换为文本块，无合法末块时省略该处断点，不插入空块，也不移到更早的消息。Messages 没有非空系统提示时省略系统断点，`automatic` 的顶层断点仍发送。

## 费用估算

每次请求结束时，请求网关在交付终态报告前，按这次请求固定的 provider 与模型标识重读当前有效价格，与用量比对后计算费用，随终态报告一次性交给调用方，保存在这次尝试上。取价沿用[元数据的来源与合并](models.md#元数据的来源与合并)，本地文件读取放在阻塞执行边界。读取记录时不重新比对或计算；之后价格变化（包括在 `providers.toml` 中修正）不回头修改旧记录。

价格单位是美元每百万 token。只有计数已报告且对应单价存在的项计入金额，不借用其他项的单价。参与计价的四项（input、cache_read、cache_write、output）里没有一项同时有单价和已报告的计数时，费用未知；否则使用下式，缺失计数仅在公式内按 0：

```text
未命中输入 = max(0, input − cache_read − cache_write)
费用（美元）= ( 未命中输入 × cost.input
             + cache_read × cost.cache_read
             + cache_write × cost.cache_write
             + output × cost.output
             ) ÷ 1,000,000
```

- reasoning 包含在 output 中，按 `cost.output` 计费。
- 费用结果保存为可选的 `cost: CostEstimate { usd, exact }`：`usd` 以 `f64` 美元保存，编码与解码均要求有限且非负，保存时不按美分舍入。
- 格式化的费用展示先合计原始金额，再按合计值与 `exact` 区分：确切金额（`exact=true`）为 0 时显示 `$0.00`，大于 0 且不到一美分时显示 `<$0.01`，其余金额按一美分舍入并显示 `$x.xx`；估算金额（`exact=false`）在相同格式前加 `≈`，例如 `≈$0.00`、`≈<$0.01`、`≈$0.04`。费用未知时，TUI statusline 显示 `$?`，CLI 金额格显示 `-`，CLI 帮助说明 `-` 表示未知。JSON 与结构化详情中的数值保留原值。
- 有可计价项时，四项比对都确切则金额确切；有价无计数或计数大于 0 却无价时仍给出金额，标为估算。估算可能偏高也可能偏低：例如 input=1000，其中 800 为缓存读，input 价为 3、缓存读价为 0.3，完整费用为 `(200×3 + 800×0.3) / 1,000,000 = $0.00084`，缺缓存读计数时估算为 `1000×3 / 1,000,000 = $0.003`；缺缓存写计数时，写入 token 留在 input 中，而写入价通常更高（例如为 input 价的 1.25 倍），会少算。因此 `≈` 不承诺偏差方向。
- 参与计价的四项中，已存在的单价必须有限且非负，明确的 0 有效。任一单价无效或计算结果超出有限金额范围时，费用未知。计费差额钳到 0，原始用量保持不变，计算不得下溢或溢出崩溃。
- 完整回复与批次拒绝按同一公式计算。流中断（含取消、deadline、provider 错误）时用量强制为 partial；已有用量仍计算金额，但标为估算。没有用量时为 partial、费用未知，不读取价格。Sent 后执行者消失、EOF 时没有收到 Finished 的尝试也保留 partial、未知费用。
- 结束时 provider 不存在、配置读取失败或无法取得有效价格时，费用未知，保留用量、正文与真实结局，只记录脱敏诊断；不回退到请求开始时的价格，不因估价失败重试模型。
- 一轮与会话的费用都是各尝试已知金额之和。各次尝试均确切、没有未知且合计未溢出时，合计确切；有估算或未知尝试时，合计为估算，未知尝试不贡献金额，也不另加未计费注记；全部未知时合计为未知。
- 按上下文长度分档的价格（models.dev 的 `tiers`、`context_over_200k`）不计入；确切只表示跟价格表比对完整，分档价格不在其中。费用只按模型价格计算，不因 provider 或认证方式区别；`chatgpt` 的模型取 `openai` 条目的价格，同样显示。

### 用量与价格表的比对

比对项只有 input、output、cache_read、cache_write。价格表是请求结束时实际取得的 `cost`：`providers.toml` 按字段覆盖 models.dev 缓存，不可用时使用内置快照，不另维护按 provider 区分的比对表。每项按下表判定，用量不全只指有价无计数：

| 价格 | 计数 | 用量完整性 | 费用 |
| --- | --- | --- | --- |
| 有 | 已报告（含 0） | complete | 计入，确切 |
| 有 | 未报告 | partial | 该项按 0 计入，估算 |
| 无 | 已报告且大于 0 | complete | 不计入该项，估算，不借用其他项单价 |
| 无 | 已报告为 0 或未报告 | complete | 对此模型不存在，确切 |

字段出现且非 null 即为已报告，明确的 0 也算；generateContent 只在[用量换算](#用量换算)规定的完成边界补零。reasoning 包含在 output 中，不单独比对。

没有一项能同时配上单价和计数时，费用未知，不显示 `≈$0.00`；用量完整性照常逐项判定，例如价格表四项均无价时用量为 complete。只有 reasoning 计数时也没有可计价项，费用未知。provider 不存在或配置读取失败时无法比对，用量为 partial、费用未知。整次尝试没有用量或流中断时，按上文的 partial 规则处理。比对结果保存在尝试的 `usage_kind` 中，与 `cost` 一起交付，读取时不重新判定。
