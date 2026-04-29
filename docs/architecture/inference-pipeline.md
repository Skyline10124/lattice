# 推理链路

## chat() 流程

```
chat(resolved, messages, tools)
  → 模型名匹配策略（DeepSeek → thinking: enabled）
  → TransportDispatcher 分发协议
  → normalize_request(messages, tools) → API 原生格式
  → POST {base_url}{endpoint} + auth header
  → reqwest-eventsource → SSE stream
  → SseParser → StreamEvent (Token / Reasoning / ToolCall / Done / Error)
```

## 协议差异

| | OpenAI | Anthropic |
|---|--------|-----------|
| 端点 | `/chat/completions` | `/v1/messages` |
| 认证 | `Authorization: Bearer` | `x-api-key` |
| 流式格式 | `data: {json}\n\n` | named SSE events |
| 工具调用 | delta.tool_calls[index] | content_block_delta |
| 思考 | delta.reasoning_content | thinking_delta |

## chat_complete() 流程

```
chat_complete() → chat() → 消费 stream → 聚合 ChatResponse
  - Token → content
  - Reasoning → reasoning_content  
  - ToolCallStart/Delta/End → tool_calls
  - Done → finish_reason + usage
  - Error → 如果是 "Stream ended" 且有内容 → 正常结束
```

## 超时

- connect_timeout: 10s
- 无全局 timeout（已移除，不杀长流式响应）
