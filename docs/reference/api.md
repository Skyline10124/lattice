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
    pub fn with_sandbox(self, config: SandboxConfig) -> Self;

    /// 自动 tool loop：反复调用 LLM，自动执行工具，直到 LLM 不再请求工具或达到上限
    pub async fn run(&mut self, content: &str) -> Result<RunResult, ArtemisError>;

    /// 单次发送消息（手动 tool loop）
    pub fn send(&mut self, content: &str) -> Vec<LoopEvent>;
    pub fn submit_tools(&mut self, results: Vec<(String, String)>, max_size: Option<usize>) -> Vec<LoopEvent>;
}

/// Tool 执行器 trait
pub trait ToolExecutor {
    fn execute(&self, name: &str, args: &str) -> Result<String, ToolError>;
}

/// Agent 间派发 trait（agent_call 工具）
pub trait AgentDispatcher {
    fn dispatch(&self, name: &str, prompt: &str) -> Result<String, AgentError>;
}

pub struct SandboxConfig {
    pub allowed_paths: Vec<String>,
    pub allowed_commands: Vec<String>,
    pub allowed_domains: Vec<String>,
}

pub enum LoopEvent {
    Token { text: String },
    Reasoning { text: String },
    ToolCallRequired { calls: Vec<ToolCall> },
    Done { usage: Option<TokenUsage> },
    Error { message: String },
}
```

### 17 个内置工具

read_file, grep, write_file, list_directory, run_test, run_clippy, bash, patch, run_command, list_processes, web_search, web_fetch, browser_navigate, browser_screenshot, browser_console, execute_code, agent_call

### Context Trimming

`AgentState::trim_messages` 在 token 超限时自动裁剪最早的消息，保留 system prompt 和最近的消息。

## artemis-harness

```rust
/// 从 TOML 文件加载 Agent 配置
pub struct AgentProfile {
    pub name: String,
    pub model: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
}

pub struct AgentRegistry {
    pub fn load(path: &str) -> Result<Self>;
    pub fn get(&self, name: &str) -> Option<&AgentProfile>;
}

pub struct AgentRunner {
    /// 创建 AgentRunner（自动加载 SQLite 记忆并回放历史）
    pub fn new(profile: &AgentProfile) -> Result<Self>;
    pub async fn run(&mut self, input: &str) -> Result<String>;
}

/// 顺序链式编排
pub struct Pipeline {
    pub fn new(stages: Vec<PipelineStage>) -> Self;
    /// skip: 条件跳过 stage
    /// fallback: stage 失败时回退到指定 stage
    pub async fn run(&mut self, input: &str) -> Result<String>;
}

/// agent_call:name 工具的 harness 层派发器
pub struct HarnessAgentDispatcher;
impl AgentDispatcher for HarnessAgentDispatcher { ... }
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
