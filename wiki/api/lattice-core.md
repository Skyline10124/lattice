# lattice-core API

模型路由、流式推理、重试策略。**无 PyO3，纯 Rust，可独立嵌入任何项目。**

## 公共 API

### 顶级函数

```rust
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError>
pub fn inspect_model(model: &str) -> Result<ResolvedModel, LatticeError>
pub fn chat(resolved: &ResolvedModel, messages: &[Message], tools: &[ToolDefinition]) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError>
pub fn chat_complete(resolved: &ResolvedModel, messages: &[Message], tools: &[ToolDefinition]) -> Result<ChatResponse, LatticeError>
pub fn chat_with_effort(resolved: &ResolvedModel, messages: &[Message], tools: &[ToolDefinition], effort_override: Option<&str>) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError>
pub fn init_logging()
pub fn init_debug_logging()
```

### 核心类型

#### ResolvedModel

```rust
pub struct ResolvedModel {
    pub canonical_id: String,      // "claude-sonnet-4-6"
    pub provider: String,          // "anthropic"
    pub api_key: Option<String>,    // masked in Debug/Serialize
    pub base_url: String,
    pub api_protocol: ApiProtocol,
    pub api_model_id: String,      // provider-specific ID
    pub context_length: u32,
    pub credential_status: CredentialStatus,
    pub thinking_effort: Option<String>,
}
```

`Debug` 和 `Serialize` 实现隐藏 `api_key`（显示 `"***"`）。

#### CredentialStatus

```rust
pub enum CredentialStatus {
    Present,
    NotRequired,
    Missing,
}
```

#### ApiProtocol

```rust
pub enum ApiProtocol {
    OpenAiChat,
    AnthropicMessages,
    GeminiGenerateContent,
    Unknown(String),  // serde fallback
}
```

支持 serde 自定义序列化：短别名 (`"openai"`, `"anthropic"`, `"gemini"`）和全名。

#### LatticeError

```rust
pub enum LatticeError {
    RateLimit { retry_after: Option<f64>, provider: String },
    Authentication { provider: String },
    ModelNotFound { model: String },
    ProviderUnavailable { provider: String, reason: String },
    ContextWindowExceeded { tokens: u32, limit: u32 },
    ToolExecution { tool: String, message: String },
    Streaming { message: String },
    Config { message: String },
    Network { message: String, status: Option<u16> },
}
```

- 所有变体 scrub credentials（从 body 移除 API key pattern）
- `RateLimit` 和 `ProviderUnavailable` 是可重试的（`ErrorClassifier::is_retryable()` 返回 true）

#### Message / Role / ToolCall / FunctionCall / ToolDefinition

```rust
pub enum Role { System, User, Assistant, Tool }

pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
    pub reasoning: Option<String>,  // thinking/extended thinking content
}

pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

pub struct FunctionCall {
    pub name: String,
    pub arguments: String,  // JSON string
}

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON schema
}
```

#### StreamEvent

```rust
pub enum StreamEvent {
    Token { content: String },
    ToolCall { id: String, function: FunctionCall },
    Done { reason: String, usage: Option<TokenUsage> },
    Error { message: String },
    Reasoning { content: String },
    ToolResult { id: String, result: String },
}
```

#### TokenUsage

```rust
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}
```

### ModelRouter

```rust
pub struct ModelRouter;

impl ModelRouter {
    pub fn new() -> Self;
    pub fn with_credentials(creds: HashMap<String, String>) -> Self;
    pub fn resolve(&self, model_name: &str, provider_override: Option<&str>) -> Result<ResolvedModel, LatticeError>;
    pub fn inspect_model(&self, model_name: &str) -> Result<ResolvedModel, LatticeError>;
    pub fn register_model(&mut self, entry: ModelCatalogEntry);
    pub fn list_models(&self) -> Vec<String>;
    pub fn list_authenticated_models(&self) -> Vec<String>;
    pub fn normalize_model_for_provider(&self, canonical_id: &str, provider_id: &str) -> String;
    pub fn clear_credential_cache(&self);
}
```

resolve() 内部流程：normalize → alias → catalog lookup → provider priority sort → credential check → base_url resolve → security validation

### Transport trait

```rust
pub trait Transport: Send + Sync {
    fn normalize_request(&self, request: &ChatRequest) -> Result<serde_json::Value, TransportError>;
    fn denormalize_response(&self, response: &serde_json::Value) -> Result<ChatResponse, TransportError>;
    fn normalize_messages(&self, messages: &[Message]) -> NormalizedMessages;
    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Vec<Value>;
    fn chat_endpoint(&self) -> &str;
    fn auth_header_name(&self) -> &str;
    fn auth_header_value(&self, api_key: &str) -> String;
    fn apply_auth_to_request(&self, req: RequestBuilder, api_key: &str) -> RequestBuilder;
    fn create_sse_parser(&self) -> Box<dyn SseParser>;
    fn apply_temperature(&self, body: &mut Value, temp: Option<f64>);
    fn set_stream_flag(&self, body: &mut Value, stream: bool);
}
```

三实现：ChatCompletionsTransport, AnthropicTransport, GeminiTransport

### TransportDispatcher

```rust
pub struct TransportDispatcher;

impl TransportDispatcher {
    pub fn new() -> Self;  // pre-loaded with 3 default transports
    pub fn register(&mut self, protocol: ApiProtocol, transport: Box<dyn Transport>);
    pub fn dispatch(&self, protocol: &ApiProtocol) -> Option<&dyn Transport>;
    pub fn dispatch_for_resolved(&self, resolved: &ResolvedModel) -> Option<&dyn Transport>;
}
```

### SseParser trait

```rust
pub trait SseParser: Send + Sync {
    fn parse_chunk(&mut self, event_type: &str, data: &str) -> Result<Vec<StreamEvent>, Box<dyn Error>>;
}
```

三实现：OpenAiSseParser, AnthropicSseParser, GeminiSseParser。所有 parser 是 **stateful**——跨 chunk 跟踪 tool call ID。

### streaming 公共函数

```rust
pub fn sse_from_bytes_stream(
    body: impl Stream<Item = Result<impl AsRef<[u8]>, impl Display>> + Send + 'static,
    parser: Box<dyn SseParser>,
) -> impl Stream<Item = StreamEvent>;

pub fn parse_raw_sse(input: &str) -> Vec<RawSseEvent>;
pub fn parse_sse_text(input: &str, parser: &mut dyn SseParser) -> Result<Vec<StreamEvent>>;
pub fn chat_response_to_stream(response: ChatResponse) -> Vec<StreamEvent>;
```

> **Limits**: Hardcoded `MAX_SSE_EVENTS=10000`, `MAX_SSE_DATA_SIZE=1MB`, `MAX_SSE_BUFFER_SIZE=10MB`. See [重构 C8](../refactoring/lattice-core.md#c8).

### ErrorClassifier

```rust
pub struct ErrorClassifier;

impl ErrorClassifier {
    pub fn is_retryable(error: &LatticeError) -> bool;
    pub fn classify(status_code: u16, response_body: &str, provider: &str) -> LatticeError;
    pub fn classify_with_retry_header(status_code: u16, body: &str, provider: &str, retry_after: Option<&str>) -> LatticeError;
}
```

### TokenEstimator

```rust
pub fn estimate_tokens(text: &str, model: &str) -> u32;
```

使用 tiktoken 对 OpenAI 模型，char/4 估算对其他模型。

### RetryPolicy

```rust
pub struct RetryPolicy {
    pub max_retries: u32,        // default: 3
    pub base_delay_ms: u64,      // default: 1000
    pub max_delay_ms: u64,       // default: 60000
    pub jitter: bool,            // default: true
}
```

### invocation 内部函数

```rust
pub(crate) fn validate_injected_header(header_name: &str) -> Result<(), LatticeError>;
pub(crate) fn send_streaming_request(...) -> Result<...>;
```

## Catalog 类型

```rust
pub struct ModelCatalogEntry {
    pub canonical_id: String,
    pub providers: Vec<CatalogProviderEntry>,
    pub aliases: Vec<String>,
}

pub struct CatalogProviderEntry {
    pub provider_id: String,
    pub api_model_id: Option<String>,
    pub base_url: Option<String>,
    pub credential_keys: Vec<String>,
    pub priority: u32,
    pub api_protocol: ApiProtocol,
}

pub struct ProviderDefaults {
    pub base_url: String,
    pub credential_key: String,
    pub api_protocol: ApiProtocol,
}

pub struct CatalogData {
    pub models: Vec<ModelCatalogEntry>,
    pub aliases: HashMap<String, String>,
    pub provider_defaults: HashMap<String, ProviderDefaults>,
}
```