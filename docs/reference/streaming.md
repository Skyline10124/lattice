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
