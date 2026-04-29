# Streaming 协议

## OpenAI SSE

```
data: {"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}
data: {"choices":[{"delta":{"content":" world"},"finish_reason":null}]}
data: {"choices":[{"delta":{},"finish_reason":"stop"}]}
data: [DONE]
```

- `[DONE]` 是纯传输信号，不产生 Done 事件
- `finish_reason` 从最后一个语义 chunk 取
- `reasoning_content` 在 delta 中优先于 content 提取

## Anthropic SSE

```
event: message_start
event: content_block_start  (type: "text" / "tool_use" / "thinking")
event: content_block_delta  (type: "text_delta" / "input_json_delta" / "thinking_delta")
event: content_block_stop
event: message_delta
```

- `thinking_delta` → `StreamEvent::Reasoning`
- stop_reason 映射：`end_turn` → `stop`, `tool_use` → `tool_calls`, `max_tokens` → `length`

## denormalize_stream_chunk（Transport trait）

`denormalize_stream_chunk` 是 `Transport` trait 上的一个方法，用于将 provider 的 SSE 事件转换为内部 `StreamEvent`。**主 `chat()` 路径不使用此方法**——它通过 `create_sse_parser()` 返回的 `SseParser` 直接解析 SSE 流。`denormalize_stream_chunk` 保留用于测试和独立的 chunk 级验证。两者可能产生不同的输出，实际推理请以 `SseParser` 路径为准。

## StreamEvent

```rust
pub enum StreamEvent {
    Token { content: String },
    Reasoning { content: String },
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, arguments_delta: String },
    ToolCallEnd { id: String },
    Done { finish_reason: String, usage: Option<TokenUsage> },
    Error { message: String },
}
```
