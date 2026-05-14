# 推理链路

## chat() 流程

```
chat(resolved, messages, tools)
  → 模型名匹配策略（DeepSeek → thinking: enabled）
  → TransportDispatcher 分发协议
    → ChatCompletionsTransport (OpenAI-like)
    → AnthropicTransport (Anthropic native)
    → GeminiTransport (Google Gemini, 非流式)
  → normalize_request(messages, tools) → API 原生格式
  → POST {base_url}{endpoint} + auth header
  → bytes_stream → sse_from_bytes_stream()
  → SseParser → StreamEvent (Token / Reasoning / ToolCall / Done / Error)
```

> Gemini 路径特殊：不走 SSE stream，而是 `generateContent` 单次 POST → 完整 JSON → `chat_response_to_stream()` 拆为 StreamEvent 序列。

## 协议差异

| | OpenAI | Anthropic | Gemini |
|---|--------|-----------|--------|
| 端点 | `/chat/completions` | `/v1/messages` | `/v1beta/models/{model}:generateContent` |
| 认证 | `Authorization: Bearer` | `x-api-key` | URL param `key=` |
| 流式格式 | `data: {json}\n\n` SSE | named SSE events | **非流式**（单次 JSON） |
| 工具调用 | delta.tool_calls[index] | content_block_delta | functionCall 字段 |
| 思考 | delta.reasoning_content | thinking_delta | thoughtsContent 字段 |
| 角色 | user/assistant | user/assistant | user/model |
| 停止原因 | finish_reason | stop_reason | finishReason |

## chat_complete() 流程

```
chat_complete() → chat() → 消费 stream → 聚合 ChatResponse
  - Token → content
  - Reasoning → reasoning_content  
  - ToolCallStart/Delta/End → tool_calls
  - Done → finish_reason + usage
  - Error → 如果是 "Stream ended" 且有内容 → 正常结束
```

## SSE 解析器

| Parser | 协议 | 状态跟踪 |
|--------|------|---------|
| OpenAiSseParser | OpenAI-like SSE | tool_call ID→index 映射 |
| AnthropicSseParser | Anthropic named events | tool_call content block 聚合 |
| — | Gemini | 无（单次响应，不需 parser） |

## SSE 硬限制

```rust
MAX_SSE_EVENTS = 10_000      // 单次 SSE 最大事件数
MAX_SSE_DATA_SIZE = 1MB      // 单个 data 字段最大尺寸  
MAX_SSE_BUFFER_SIZE = 10MB   // 解析器缓冲区上限
```

不可配置，接近限制时无 tracing 警告（参见重构 #46）。

## 超时

- connect_timeout: 10s
- read_timeout: 120s（chunk 间隔，SSE stream 中每个 chunk 重置计时器）
- 无总时长限制 — 长 streaming 响应不会被杀
- HTTP 重定向完全禁止（`Policy::none()`）— SSRF 防护

## Transport trait

```rust
trait Transport {
    fn chat_request_endpoint(&self) -> &str;
    fn normalize_request(&self, messages, tools) -> Result<RequestParts>;
    fn denormalize_stream_chunk(&self, chunk) -> Vec<StreamEvent>;
    fn create_sse_parser(&self) -> Option<Box<dyn SseParser>>;
    fn with_extra_headers(&self, headers) -> Self;
    // ... 9 方法 + default impls
}
```

`TransportBase` 提供共享 HTTP client + chat() 实现。`TransportDispatcher` 按协议分发。