---
status: active
---

# 推理档位

> 所属领域：[模型接入](README.md)

## 可用档位

模型可用的档位由元数据的 `reasoning_options` 得出。手写元数据的 `effort` 列表覆盖候选档位名字：元数据能给出某档的参数族来源时沿用该来源，按协议、features 和预算规则过滤不可表达的档位。

元数据给不出某个手写档位的来源时，按有效 features 推出写法；模型级 features 覆盖 provider 级配置：

| 有效 features | 手写档位的写法 |
| --- | --- |
| 只有 `thinking_toggle` | `off` 关闭、`high` 开启；其他档位无效 |
| 只有 `thinking_budget` | `low`、`medium`、`high`、`max` 按预算表与输出上限写预算，无元数据上下界；`off`、`minimal`、`xhigh` 无效 |
| 两者都有 | `off` 关闭；`low`、`medium`、`high`、`max` 写预算并开启；`minimal`、`xhigh` 无效 |
| 两者都未启用，或协议没有这两个 feature | 按 effort 项写档位 |

上述 feature 推断只适用于缺少元数据来源的档位。无法表达的手写档位使该模型的可用性变为 `invalid`：提交选了这个模型时，即使未选该档位也在接受前按不可重试的配置错误拒绝，诊断含 provider、模型、档位和原因；其他模型不受影响。`GatewayTurn::prepare` 与候选查询共用此校验，候选查询在 `ready_only = false` 与 `Target` 下将该候选标为 `invalid`，见[模型候选](models.md#模型候选)。上例的 `my-vllm` 用 `thinking_toggle = "enable-thinking"` 使 `off`/`high` 写开关。

| `reasoning_options` 中的项 | 增加的档位 |
| --- | --- |
| `{"type": "effort", "values": [...]}` | 列出的档位；`none` 记作 `off`，`default` 忽略 |
| `{"type": "toggle"}` | `off`；同一模型没有 `effort` 或 `budget_tokens` 项时，另加 `high`，表示只写开启、不写深度，由服务端按默认深度推理 |
| `{"type": "budget_tokens", "min": …, "max": …}`，且同一模型没有 `effort` 项 | `low`、`medium`、`high`、`max`，按下文的预算去掉重复的档位 |

- 每个档位只用产生它的那一项对应的字段表达，不跨类型回退：来自 toggle 项的 `off` 与 `high` 只写开关字段，来自 effort 项的档位（包括取值 `none` 得到的 `off`）只写档位字段，来自 budget_tokens 项的档位只写预算字段。所选协议与 features 写不出某一档时，它不出现在可用档位中。
- `reasoning` 为 true 而没有 `reasoning_options` 的模型总在推理，没有可选档位。
- 按严格规则请求模型不支持的档位时，在 `configure` 或提交准入阶段按配置错误拒绝，不降级到相近档位；换模型未显式选档位时的回退规则见[会话合同](../session/settings.md#档位规则)。
- profile 的 `[agent].effort` 设默认档位；没有档位时不发送档位参数，由 provider 使用默认推理强度。Responses、generateContent 与 Interactions 请求推理文字与是否选择档位无关，不改变推理强度；Messages 的例外见下方映射。

按 token 数限制的档位取以下预算：

| 档位 | 预算 |
| --- | --- |
| `low` | 2,048 |
| `medium` | 8,192 |
| `high` | 16,384 |
| `max` | 32,768 |

- 预算先夹到模型的 `[min, max]` 之内（缺的一端不限），再限制为不超过输出上限的一半，给正文留出空间（这是本项目自定的规则，服务端并不要求，本地也不据此拒绝请求）；输出上限缺失或为 0 时按 32,768 计，即最多 16,384。限制后为 0 时没有该预算档位。只有 Anthropic Messages 在发送时把预算提高到 `max(1024, min)`（缺少 `min` 时为 1,024），即发送 `max(限制后的预算, 1024, min)`，低于 `min` 的档位仍可用，由服务端判断请求是否可接受；其他协议限制后低于 `min` 时，该档位不可用。
- 夹紧后与低一档预算相同的档位不列出。例如输出上限为 8,192 时，只有 `low`（2,048）与 `medium`（4,096）。
- `max` 档不取模型的上限：models.dev 中 Claude 的预算上限是输出上限减 1，用满就不剩正文的空间；多数模型也没有写上限。

## 档位映射

| 协议 | 档位来自 `effort` 项 | 档位来自 `budget_tokens` 项 | 开启（toggle 项另加的 `high`） | 关闭（`off`） |
| --- | --- | --- | --- | --- |
| Chat Completions | 按 `effort_param` 写档位名 | 按 `thinking_budget` 写预算；`thinking_budget` 为 `none` 时这些档位不可用 | 按 `thinking_toggle` 写开启；`thinking_toggle` 为 `none` 时不可用 | 来自 toggle 项时按 `thinking_toggle` 写关闭，`thinking_toggle` 为 `none` 时不可用；来自 effort 项时按 `effort_param` 写 `"none"` |
| Responses | `reasoning: {"effort": "<档位>"}` | 不可用 | 不可用 | 来自 effort 项时 `reasoning: {"effort": "none"}`；来自 toggle 项时不可用 |
| Anthropic Messages | `thinking: {"type": "adaptive"}` 与 `output_config: {"effort": "<档位>"}` | `thinking: {"type": "enabled", "budget_tokens": N}` | `thinking: {"type": "adaptive"}` | `thinking: {"type": "disabled"}` |
| generateContent | `generationConfig.thinkingConfig.thinkingLevel` | `thinkingConfig.thinkingBudget: N` | `thinkingConfig.thinkingBudget: -1` | `thinkingConfig.thinkingBudget: 0` |
| Interactions | `generation_config.thinking_level`（`minimal`、`low`、`medium`、`high`） | 不可用 | 不可用 | 不可用 |

Responses 仅在模型元数据明确 `reasoning = true` 且档位不是 `off` 时请求推理摘要：没选档位写 `reasoning: {"summary":"auto"}`；选了档位则与 effort 合并为 `reasoning: {"effort":"<档位>","summary":"auto"}`。`false` 或没有该标志时不请求摘要，选中的 effort 照常发送；`off` 只写 `reasoning: {"effort":"none"}`。摘要请求与是否选择档位无关，不改变推理强度。

Interactions 仅在模型元数据明确 `reasoning = true` 时发送 `generation_config.thinking_summaries: "auto"`，未选档位也发送；选择档位时与 `thinking_level` 合并在同一个对象里。generateContent 在模型元数据明确 `reasoning = true` 且推理未关闭时发送 `generationConfig.thinkingConfig.includeThoughts: true`，与已有 `thinkingLevel` 或 `thinkingBudget` 合并；未选档位时只发送 includeThoughts，`off`、toggle 关闭或 `thinkingBudget: 0` 均不发送。`false` 或没有该标志时，两种协议均不请求推理文字，所选档位照常发送。

Messages 仅在模型元数据明确 `reasoning = true` 时给 adaptive 对象附加 `display: "summarized"`，包括 effort 档位和 toggle 开启的 adaptive；`false` 或没有该标志时不附加 display。budget 对应较早模型，默认返回摘要，不附加 display，disabled 也不附加。未选档位时不发 `thinking`：该字段会开启默认不开推理的模型，改变推理强度，不能仅为取得文字而发送。

Chat Completions 中，元数据给出来源的档位在 `reasoning_options` 含 toggle 项且 `thinking_toggle` 不为 `none` 时，同时按 `thinking_toggle` 写开启（`off` 除外）；没有 toggle 项时不写开关，例如 Kimi K3.1 Pro 只接受 `reasoning_effort`。缺少元数据来源的手写档位按[有效 features](#可用档位)推断写法。某个档位在所选协议上不可用时，它不出现在可用档位中。

当前 `claude-opus-4-5` 不支持 adaptive，选择任何档位都会返回 HTTP 400；请不选档位，或改用 Claude Opus 4.6 及以上。该模型退役后删除此注记。
