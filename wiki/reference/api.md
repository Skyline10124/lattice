# API 参考

## lattice-core

```rust
/// 解析模型名到连接详情
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError>;

/// 流式推理
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<impl Stream<Item = StreamEvent>, LatticeError>;

/// 非流式推理（内部调 chat + 收集）
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, LatticeError>;
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

## lattice-agent

> **Known issue**: `Agent.send_message()` currently hangs in synchronous contexts (`fn main` without `#[tokio::main]`). Being tracked.

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
    pub async fn run(&mut self, content: &str) -> Result<RunResult, LatticeError>;

    /// 单次发送消息（手动 tool loop）
    pub fn send(&mut self, content: &str) -> Vec<LoopEvent>;
    pub fn submit_tools(&mut self, results: Vec<(String, String)>, max_size: Option<usize>) -> Vec<LoopEvent>;
}

/// Tool 执行器 trait
pub trait ToolExecutor {
    fn execute(&self, name: &str, args: &str) -> Result<String, ToolError>;
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

### 7 个内置工具

read_file, grep, write_file, list_directory, bash, patch, web_search

### Context Trimming

`AgentState::trim_messages` 在 token 超限时自动裁剪最早的消息，保留 system prompt 和最近的消息。

## lattice-bus（编排层）

```rust
/// 从 TOML 文件加载 Agent 配置
pub struct AgentProfile {
    pub agent: AgentConfig,
    pub system: SystemConfig,
    pub tools: ToolsConfig,
    pub plugins: Option<PluginsConfig>,
}

pub struct AgentRegistry {
    pub fn load_dir(dir: &Path) -> Result<Self>;
    pub fn get(&self, name: &str) -> Option<&AgentProfile>;
}

pub struct AgentRunner {
    pub fn from_profile(profile: AgentProfile, agent: Agent) -> Self;
    pub async fn run(&mut self, input: &str, max_turns: u32) -> Result<Value>;
}

/// 顺序链式编排 + handoff 规则引擎
pub struct Pipeline {
    pub fn new(name: &str, registry: Arc<AgentRegistry>, memory: Option<Arc<dyn Memory>>, event_bus: Option<Arc<EventBus>>) -> Self;
    pub async fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun;
}

/// 插件 DAG 编排
pub struct PluginDagRunner {
    pub fn new(config: &PluginsConfig, plugin_registry: &PluginRegistry, tool_registry: &ToolRegistry, retry_policy: RetryPolicy, shared_memory: Option<Arc<dyn Memory>>) -> Self;
    pub async fn run(&mut self, initial_input: &str, default_model: &str) -> Result<Value, DAGError>;
}
```

位置：`lattice-bus/src/pipeline.rs`（Pipeline）、`lattice-bus/src/runner.rs`（AgentRunner）、`lattice-plugin/src/dag_runner.rs`（PluginDagRunner）、`lattice-plugin/src/orchestration.rs`（handoff 规则与 DAG 配置类型）。

## lattice-python

```python
class LatticeEngine:
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
