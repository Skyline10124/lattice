# lattice-agent API

异步 Agent 对话、工具执行、沙箱安全、内存管理。

## 公共 API

### ToolExecutor trait

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> String;
}
```

### PluginAgent trait

```rust
#[async_trait(?Send)]
pub trait PluginAgent {
    async fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>>;
    fn set_system_prompt(&mut self, _prompt: &str);
    fn set_system_prompt_delta(&mut self, _delta: Option<SystemPromptDelta>);
    fn set_output_contract_delta(&mut self, _delta: Option<SystemPromptDelta>);
    fn add_tools(&mut self, _tools: Vec<ToolDefinition>);
    fn token_usage(&self) -> u64;
}
```

> **边界问题**: 此 trait 定义在 agent crate 但本质上属于 plugin 的接口需求。参见 [B1](../boundary-analysis/issues.md#b1)。

### Agent struct

```rust
pub struct Agent;

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self;
    pub fn with_thinking_effort(self, effort: Option<String>) -> Self;
    pub fn with_tools(self, tools: Vec<ToolDefinition>) -> Self;
    pub fn with_retry(self, policy: RetryPolicy) -> Self;
    pub fn with_audit(self, audit: Arc<AuditLog>) -> Self;
    pub fn with_memory(self, memory: Arc<dyn Memory>) -> Self;
    pub fn with_tool_executor(self, executor: Box<dyn ToolExecutor>) -> Self;
    pub fn with_blob_store(self, store: Arc<BlobStore>) -> Self;  // feature-gated
    pub fn with_provider(self, provider: impl ContextProvider + 'static) -> Self;
    pub fn inject_bus_events(&mut self, events: Vec<ContextEvent>);
    pub fn set_system_prompt(&mut self, prompt: &str);
    pub fn set_system_prompt_delta(&mut self, delta: Option<SystemPromptDelta>);
    pub fn set_output_contract_delta(&mut self, delta: Option<SystemPromptDelta>);
    pub fn add_tools(&mut self, tools: Vec<ToolDefinition>);
    pub fn set_behavior(&mut self, mode: BehaviorMode);
    pub fn behavior(&self) -> &BehaviorMode;
    pub fn messages(&self) -> &[Message];
    pub fn token_usage(&self) -> u64;
    pub fn seed_messages(&mut self, messages: Vec<Message>);

    // 对话方法
    pub async fn send_message(&mut self, content: &str) -> Vec<LoopEvent>;
    pub async fn submit_tools(&mut self, results: Vec<(String, String)>, max_size: Option<usize>) -> Vec<LoopEvent>;
    pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent>;
    pub async fn run_streaming<F>(&mut self, content: &str, max_turns: u32, emit: F) -> Vec<LoopEvent>
        where F: FnMut(LoopEvent);
    pub async fn with_permissive<Fut, T>(&mut self, f: impl FnOnce() -> Fut) -> Result<T, LatticeError>;
}
```

### LoopEvent

```rust
pub enum LoopEvent {
    // 详细变体见源码，包含 Token/ToolCall/Done/Error/Reasoning 等
}
```

### AgentState

```rust
pub struct AgentState;

impl AgentState {
    pub fn new(resolved: ResolvedModel) -> Self;
    pub fn push_system_message(&mut self, content: &str);
    pub fn push_user_message(&mut self, content: &str);
    pub fn push_assistant_message(&mut self, content: &str, reasoning: &str, tool_calls: Option<Vec<ToolCall>>);
    pub fn push_tool_result(&mut self, call_id: &str, result: &str, max_size: Option<usize>);
    pub fn seed_messages(&mut self, messages: Vec<Message>);
    pub fn replace_first_user_message(&mut self, content: &str);
    pub fn upsert_user_message_from(&mut self, start: usize, content: &str);
    pub fn add_token_usage(&mut self, tokens: u64);
    pub fn trim_messages(&mut self, context_length: u32, safety_margin_percent: u8);
}
```

### SandboxConfig

```rust
pub struct SandboxConfig {
    pub landlock: LandlockConfig,
    pub hook_chain: Option<HookChain>,
    pub allowed_commands: Vec<String>,
    pub allowed_write_dirs: Vec<String>,
    pub blocked_commands: Vec<String>,
    pub max_http_response_size: usize,
    pub audit_log: Option<Arc<AuditLog>>,
}

impl SandboxConfig {
    pub fn permissive() -> Self;     // 无路径/命令限制（仅 metachar + URL check）
    pub fn project_dirs(write_dirs: Vec<String>, dev_commands: Vec<String>) -> Self;
    pub fn check_read(&self, path: &str) -> Result<(), String>;
    pub fn check_write(&self, path: &str) -> Result<(), String>;
    pub fn check_command(&self, cmd: &str) -> Result<(), String>;
    pub fn check_url(&self, url_str: &str) -> Result<(), String>;
}
```

4 层安全检查顺序：HookChain → metacharacters → env overrides → command allowlist

### ToolHook trait / HookChain

```rust
pub trait ToolHook: Send + Sync {
    fn name(&self) -> &str;
    fn pre_execute(&self, tool: &str, command: &str) -> Result<String, HookDenyReason>;
    fn post_execute(&self, tool: &str, command: &str, exit_code: i32, stdout: &str, stderr: &str) -> Option<HookDenyReason>;
}

pub struct HookDenyReason {
    pub hook: String,
    pub reason: String,
    pub severity: HookSeverity,
}

pub enum HookSeverity { Warning, Deny, Lockdown }

pub struct HookChain { hooks: Vec<Arc<dyn ToolHook>> }
```

两个内置 hook：TirithHook（阻止危险命令模式）和 VetHook（拦截 curl 下载，将 HTTP→HTTPS）。

### BehaviorMode

```rust
pub enum BehaviorMode {
    Strict { confidence_threshold: f64, max_retries: u32, escalate_to: Option<String> },
    Yolo { enforce_sandbox: YoloSandboxPolicy },
}

pub enum YoloSandboxPolicy {
    EnforceCommandAllowlist,  // default: still uses allowlist (grep/find/ls/ps)
    NoBash,                   // completely disable bash tool
    Custom { allowed_commands: Vec<String> },  // custom allowlist
}
```

TOML mapping: `mode = "yolo"` + `enforce_sandbox = "allowlist" | "nobash"`

> **Note**: `BehaviorMode` and `YoloSandboxPolicy` are now shared through `lattice-core::types`.

### DefaultToolExecutor

```rust
pub struct DefaultToolExecutor;

impl DefaultToolExecutor {
    pub fn new(project_root: &str) -> Result<Self, String>;
    pub fn new_with_registry(project_root: &str, registry: Option<Arc<dyn RegistryToolAccess>>) -> Result<Self, String>;
    pub fn with_sandbox(self, config: SandboxConfig) -> Self;
    pub fn new_with_blob_store(project_root: &str, blob_store: Option<Arc<BlobStore>>) -> Result<Self, String>;
}
```

7 个默认工具：read_file, grep (纯 Rust regex), write_file, list_directory, bash, patch, web_search

bash 工具在 Linux 上使用 Landlock 文件隔离 + 资源限制（RLIMIT），非 Unix 平台无沙箱。

### Memory trait

```rust
pub trait Memory: Send + Sync {
    async fn search(&self, query: &str, limit: usize) -> Vec<MemoryEntry>;
    async fn store(&self, entry: MemoryEntry);
    async fn entries(&self) -> Vec<MemoryEntry>;
}

pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub timestamp: String,
    pub metadata: HashMap<String, String>,
}
```

实现：InMemoryMemory（HashMap），SqliteMemory（FTS5，feature-gated）

### ToolRegistry

```rust
pub struct ToolRegistry;

impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, name: &str, definition: ToolDefinition);
    pub fn get(&self, name: &str) -> Option<&ToolDefinition>;
    pub fn all(&self) -> Vec<&ToolDefinition>;
}
```

### ToolError

```rust
pub enum ToolError {
    Execution(String),
    NotFound(String),
    InvalidArgs(String),
    McpUnreachable(String),
    Timeout,
}
```

### Prompt Engine (内部模块)

```rust
// prompt/mod.rs
pub enum Layer { System=0, Rules=1, Tools=2, Memory=3, Events=4, Input=5 };
pub enum TokenBudget { Fixed(u32), Ratio(f64), Dynamic };
pub struct PromptSection { content: String, layer: Layer, priority: u8, tokens: u32 }

pub trait ContextProvider: Send + Sync {
    fn layer(&self) -> Layer;
    fn priority(&self) -> u8;
    fn budget(&self) -> TokenBudget;
    async fn produce(&self, ctx: &AssemblyContext<'_>) -> Option<PromptSection>;
}

pub struct SystemPromptDelta {
    pub layer: Layer,
    pub content: String,
    pub extra_headers: HashMap<String, String>,
}
```

Prompt engine 编译 system prompt：按 Layer 分层，按 priority 排序，在 TokenBudget 内裁剪。

### AuditLog

```rust
pub struct AuditLog;

impl AuditLog {
    pub fn new() -> Self;
    pub fn record(&mut self, event: AuditEvent);
    pub fn entries(&self) -> &[AuditEvent];
    pub fn log_sync(&mut self, event: AuditEvent);  // async write via tokio::spawn
}
```

> **Bug**: `current_size` field is dead code (never used for rotation). `log_sync()` drops JoinHandle — async write errors silently lost. See [重构 A11](../refactoring/lattice-agent.md#a11), [A12](../refactoring/lattice-agent.md#a12).

### BlobStore (feature-gated: blob-store)

```rust
pub struct BlobStore;

impl BlobStore {
    pub fn new() -> Self;
    pub fn store(&mut self, key: &str, data: &str) -> String;
    pub fn get(&self, key: &str) -> Option<&str>;
}
```

用于 bus:fetch 工具——大型 tool output 通过 blob store 存储，返回引用而非完整内容。

### SqliteMemory (已知问题)

```rust
pub struct SqliteMemory;
impl SqliteMemory {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn try_clone(&self) -> Result<Self, MemoryError>;  // may panic via .unwrap() on connection reopen
}
```

> **Bug**: `clone_box()` panics on OOM / connection reopen failure. `save_entry()` silently discards all errors via `.ok()`. Copy-paste poisoned lock error messages reference wrong methods. See [重构 A9](../refactoring/lattice-agent.md#a9), [A10](../refactoring/lattice-agent.md#a10), [A13](../refactoring/lattice-agent.md#a13).

### MessageSource / SourceRegistry (指令层级)

```rust
pub enum MessageSource {
    SystemInstruction,  // 最高优先级 — kernel prompt, role, mission
    UserInput,          // 用户任务
    AssistantOutput,    // LLM 生成 — 不可信输入
    ToolOutput,         // 工具执行结果 — DATA, not instructions
    RetrievedContext,   // memory/sqlite 搜索 — 可能 adversarial
}

pub struct SourceRegistry { sources: Vec<MessageSource> }
```

`SourceRegistry` 与 `AgentState.messages` 平行维护，用于 prompt assembly 中按来源格式化消息：
- `ToolOutput` → `<TOOL_OUTPUT>...</TOOL_OUTPUT>` 标签包裹 + 安全提示
- `RetrievedContext` → `<RETRIEVED_CONTEXT>...</RETRIEVED_CONTEXT>` 标签包裹 + adversarial 提醒
- `System/User/Assistant` → passthrough

`source_for_layer()` 将 Prompt Layer 映射到 MessageSource：
```
System/Rules/Tools → SystemInstruction
Memory/Events → RetrievedContext
Input → UserInput
```

### Memory reflect (stub)

```rust
// memory/reflect.rs — 完整 stub，未实现
pub fn extract(_session_log: &[ConversationTurn]) -> Vec<String> { vec![] }
```

> **功能缺失**: `reflect()` 永远返回空 Vec，conversation reflection 功能未实现。
