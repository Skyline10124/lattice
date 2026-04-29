# Provider 测试矩阵

## 已实测

| Provider | 协议 | 模型 | 状态 |
|----------|------|------|------|
| deepseek | OpenAI | deepseek-v4-pro, deepseek-v4-flash | think+tool 通 |
| minimax | Anthropic | minimax-m2.7, minimax-m2.5 | think 通 |
| opencode-go | OpenAI | 12 个模型 | 全部通 |
| opencode-go | Anthropic | minimax-m2.7, minimax-m2.5 | think 通 |

## 端点正确，待实测

| Provider | base_url | 协议 |
|----------|----------|------|
| openai | `api.openai.com/v1` | OpenAI |
| anthropic | `api.anthropic.com` | Anthropic |
| copilot | `api.githubcopilot.com` | OpenAI |
| opencode-zen | `opencode.ai/zen/v1` | OpenAI/Anthropic/Gemini |
| xai | `api.x.ai/v1` | OpenAI |
| alibaba | `dashscope.aliyuncs.com/compatible-mode/v1` | OpenAI |
| moonshot | `api.moonshot.ai/v1` | OpenAI |
| kimi-coding | `api.kimi.com/coding/v1` | OpenAI |
| nous | `inference-api.nousresearch.com/v1` | OpenAI |
| nvidia | `integrate.api.nvidia.com/v1` | OpenAI |
| zai | `api.z.ai/api/paas/v4` | OpenAI |
| stepfun | `api.stepfun.ai/v1` | OpenAI |
| huggingface | `router.huggingface.co/v1` | OpenAI |
| xiaomi | `api.xiaomimimo.com/v1` | OpenAI |
| kilocode | `api.kilo.ai/api/gateway` | OpenAI |
| arcee | `api.arcee.ai/api/v1` | OpenAI |
| kimi-coding-cn | `api.moonshot.cn/v1` | OpenAI |
| minimax-cn | `api.minimax.io/v1` | OpenAI |

## 无固定端点

| Provider | 原因 |
|----------|------|
| ai-gateway | 中间层网关 |
| copilot-acp | Agent 通信协议 |
| gemini / google-gemini-cli | Gemini 原生协议 |

## OpenCode Go 全模型实测

以下 14 个模型通过 opencode-go provider 全部实测通过（`OPENCODE_GO_API_KEY`）：

glm-5.1, glm-5, kimi-k2.6, kimi-k2.5, deepseek-v4-pro, deepseek-v4-flash, mimo-v2-pro, mimo-v2-omni, mimo-v2.5-pro, mimo-v2.5, minimax-m2.7, minimax-m2.5, qwen3.6-plus, qwen3.5-plus
