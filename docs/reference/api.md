# API 参考

## artemis-core

```rust
/// 解析模型名到连接详情
pub fn resolve(model: &str) -> Result<ResolvedModel, ArtemisError>;

/// 流式推理
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<impl Stream<Item = StreamEvent>, ArtemisError>;

/// 非流式推理（内部调 chat + 收集）
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, ArtemisError>;
```

### StreamEvent

```rust
pub enum StreamEvent {
    Token { content: String },
    Reasoning { content: String },      // DeepSeek/Minimax 思考链
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, arguments_delta: String },
    ToolCallEnd { id: String },
    Done { finish_reason: String, usage: Option<TokenUsage> },
    Error { message: String },
}
```

### 核心类型

- `ResolvedModel` — 解析后的模型信息（provider, api_key, base_url, protocol）
- `Message` — 消息（role, content, tool_calls, reasoning_content）
- `ChatResponse` — 完整响应（content, tool_calls, usage, reasoning_content）
- `ToolDefinition` — 工具定义（name, description, parameters JSON Schema）

## artemis-agent

```rust
pub struct Agent;

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self;
    pub fn with_tools(self, tools: Vec<ToolDefinition>) -> Self;
    pub fn with_retry(self, policy: RetryPolicy) -> Self;
    pub fn with_memory(self, memory: Box<dyn Memory>) -> Self;
    pub fn with_token_pool(self, pool: Box<dyn TokenPool>) -> Self;

    pub fn send(&mut self, content: &str) -> Vec<LoopEvent>;
    pub fn submit_tools(&mut self, results: Vec<(String, String)>, max_size: Option<usize>) -> Vec<LoopEvent>;
}

pub enum LoopEvent {
    Token { text: String },
    Reasoning { text: String },
    ToolCallRequired { calls: Vec<ToolCall> },
    Done { usage: Option<TokenUsage> },
    Error { message: String },
}
```

## artemis-python

```python
class ArtemisEngine:
    def resolve_model(self, model: str) -> PyResolvedModel
    def list_models(self) -> list[str]
    def list_authenticated_models(self) -> list[str]

class PyResolvedModel:
    canonical_id: str
    provider: str
    api_model_id: str
    context_length: int
```

Python 目前只支持 resolve，chat/streaming 待实现。
