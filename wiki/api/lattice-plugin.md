# lattice-plugin API

Plugin trait、DAG 编排、11 内置插件、handoff 规则引擎。

## 公共 API

### Plugin trait

```rust
pub trait Plugin: Send + Sync {
    type Input: Serialize + DeserializeOwned + Send;
    type Output: Serialize + DeserializeOwned + Send;

    fn name(&self) -> &str;
    fn system_prompt(&self) -> String;
    fn system_prompt_delta(&self) -> Option<SystemPromptDelta>;  // default: wraps system_prompt()
    fn to_prompt(&self, input: &Self::Input) -> String;
    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError>;
    fn tools(&self) -> Vec<ToolDefinition>;
    fn preferred_model(&self) -> Option<String>;
    fn output_schema(&self) -> Option<serde_json::Value>;  // default: None
}
```

### Behavior trait

```rust
pub trait Behavior: Send + Sync {
    fn decide(&self, confidence: f64) -> Action;
    fn on_error(&self, error: &str, attempt: u32) -> ErrorAction;
}

pub enum Action { Done, Retry }
pub enum ErrorAction { Retry, Abort, Escalate }
```

两个实现：
- `StrictBehavior` — confidence_threshold, max_retries, escalate_to
- `YoloBehavior` — 总是返回 Action::Done

### ErasedPlugin trait

```rust
pub trait ErasedPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> String;
    fn system_prompt_delta(&self) -> Option<SystemPromptDelta>;
    fn to_prompt_json(&self, context: serde_json::Value) -> String;
    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError>;
    fn tools(&self) -> Vec<ToolDefinition>;
    fn preferred_model(&self) -> Option<String>;
    fn output_schema(&self) -> Option<serde_json::Value>;
}
```

Blanket impl: `ErasedPlugin for T where T: Plugin, T::Input: DeserializeOwned, T::Output: Serialize`

> **重构机会**: `ErasedPluginAdapter<'a, P>` (lib.rs:237-268) 是 `&P` 到 `ErasedPlugin` 的包装器，但 blanket impl 已覆盖 `T: Plugin`。两者逻辑相同。参见 [重构 #](../refactoring/overview.md)。

### PluginBundle

```rust
pub struct PluginBundle {
    pub meta: PluginMeta,
    pub plugin: Box<dyn ErasedPlugin>,
    pub default_behavior: BehaviorMode,
    pub default_tools: Vec<ToolDefinition>,
}

pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
}

fn behavior_to_behavior_trait(mode: &BehaviorMode) -> Box<dyn Behavior>;
fn effective_sandbox(behavior: &BehaviorMode, base: &SandboxConfig) -> SandboxConfig;
```

### PluginRegistry

```rust
pub struct PluginRegistry;

impl PluginRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, bundle: PluginBundle) -> Result<(), RegistryError>;
    pub fn replace(&mut self, bundle: PluginBundle);
    pub fn remove(&mut self, name: &str);
    pub fn replace_all(&mut self, bundles: Vec<PluginBundle>);
    pub fn names(&self) -> Vec<String>;
    pub fn get(&self, name: &str) -> Option<Arc<PluginBundle>>;
    pub fn list(&self) -> Vec<Arc<PluginBundle>>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

pub enum RegistryError { DuplicateName(String) }
```

> **重构机会**: RwLock poison recovery 重复 9 次。参见 [重构 #8](../refactoring/overview.md)。

### PluginRunner / ErasedPluginRunner

```rust
pub struct PluginRunner<'a, P, B, A>;
impl PluginRunner {
    pub fn new(plugin: &'a P, behavior: &'a B, agent: &'a mut A, config: PluginConfig, hooks: &'a dyn PluginHooks, retry_policy: RetryPolicy, memory: Option<Arc<dyn Memory>>) -> Self;
    pub fn run(self, input: P::Input) -> Result<RunResult, PluginError>;
}

pub struct ErasedPluginRunner<'a>;
impl ErasedPluginRunner {
    pub fn new(plugin: &'a dyn ErasedPlugin, behavior: &'a dyn Behavior, agent: &'a mut dyn PluginAgent, config: PluginConfig, hooks: &'a dyn PluginHooks, retry_policy: RetryPolicy, memory: Option<Arc<dyn Memory>>) -> Self;
    pub fn run(self, context: serde_json::Value) -> Result<RunResult, PluginError>;
}
```

> **重构机会**: 8 参数构造器。参见 [重构 #29](../refactoring/overview.md)。

### PluginConfig

```rust
pub struct PluginConfig {
    pub max_turns: u32,            // default: 10
    pub max_output_bytes: usize,   // default: 1MB
    pub context_check: bool,       // default: true
    pub timeout_per_call_secs: u64, // default: 120
}
```

### PluginError

```rust
pub enum PluginError {
    Parse(String),
    Validation(String),
    MissingTool(String),
    ContextExceeded(u32),
    MaxTurnsExceeded(u32),
    OutputTooLarge(usize, usize),
    Escalated { original: String, after_attempts: u32 },
    Other(String),
}
```

### RunResult

```rust
pub struct RunResult {
    pub output: String,
    pub turns: u32,
    pub final_action: Action,
}
```

### Handoff 规则引擎

```rust
pub struct HandoffCondition {
    pub field: String,    // "confidence", "issues[any].severity"
    pub op: String,       // "==", "!=", "<", ">", "<=", ">=", "contains"
    pub value: serde_json::Value,
}

pub struct HandoffRule {
    pub condition: Option<HandoffCondition>,
    pub all: Option<Vec<HandoffRule>>,
    pub any: Option<Vec<HandoffRule>>,
    pub default: Option<bool>,
    pub target: HandoffTarget,
}

pub enum HandoffTarget {
    Single(String),
    Fork(Vec<String>]),   // TOML: "fork:A,B"
}

pub fn eval_rules(rules: &[HandoffRule], output: &serde_json::Value) -> Option<HandoffTarget>;
```

规则按顺序检查，首个匹配获胜。支持 `[any]` 迭代数组元素。

### PluginDagRunner

```rust
pub struct PluginDagRunner<'a>;

impl PluginDagRunner<'a> {
    pub fn new(config: &'a PluginsConfig, plugin_registry: Arc<PluginRegistry>, tool_registry: Arc<ToolRegistry>, retry_policy: RetryPolicy, shared_memory: Option<Arc<dyn SharedMemory>>) -> Self;
    pub fn with_registry_tool_access(self, registry: Arc<dyn RegistryToolAccess>) -> Self;
    pub fn with_credentials(self, credentials: HashMap<String, String>) -> Self;
    pub fn find_edge(&self, from: &str, output: &serde_json::Value) -> Option<HandoffTarget>;
    pub fn run(&mut self, initial_input: &str, default_model: &str) -> Result<serde_json::Value, DAGError>;
}
```

> **边界问题**: 直接构造 Agent。参见 [B2](../boundary-analysis/issues.md#b2)。

### DAGError

```rust
pub enum DAGError {
    EntrySlotNotFound,
    SlotNotFound(String),
    PluginNotFound(String),
    Resolve(LatticeError),
    MaxSlotTransitionsExceeded,
    Plugin { slot: String, source: String },
    OutputParse,
    ForkNotSupportedInDag,
    MissingPluginRegistry,
    MissingToolRegistry,
    ToolExecutor(String),
}
```

MAX_DAG_SLOT_TRANSITIONS = 50

### PluginsConfig

```rust
pub struct PluginsConfig {
    pub entry: String,
    pub slots: Vec<PluginSlotConfig>,
    pub edges: Vec<AgentEdgeConfig>,
    pub shared_tools: Vec<String>,
}

pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,
    pub tools: Option<Vec<String>>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorMode>,
}

pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
}
```

> **Note**: current DAG configuration uses `DagEdgeConfig`.

### PluginManifest

```rust
pub fn load_manifest(path: &Path) -> Result<PluginBundle, ManifestError>;

pub enum ManifestError {
    Io { path: String, source: io::Error },
    Toml { path: String, source: toml::de::Error },
    Invalid { path: String, message: String },
}
```

### PluginLoader

```rust
pub fn load_dir(dir: &Path) -> Result<Vec<PluginBundle>, LoaderError>;

pub enum LoaderError {
    Io { path: String, source: io::Error },
    Manifest(ManifestError),
    DuplicateName { name: String, first: String, second: String },
}
```

### PluginWatcher

```rust
pub struct PluginWatcher { _handle: JoinHandle<()> };

impl PluginWatcher {
    pub fn spawn(dir: &Path, registry: Arc<RwLock<PluginRegistry>>, include_builtins: bool) -> Result<Self, Box<dyn Error>>;
}
```

> **问题**: 无 shutdown 方法，watcher 线程永久运行。

### 11 内置插件

| 插件 | Input | Output | 特殊 |
|------|-------|---------|------|
| CodeReview | `CodeReviewInput { input, file_path, context_rules }` | `CodeReviewOutput { issues, confidence }` | — |
| Refactor | `RefactorInput { code, review, instructions }` | `RefactorOutput { refactored_code, changes }` | 依赖 CodeReviewOutput |
| TestGen | `TestGenInput { code, language, focus_areas }` | `TestGenOutput { tests, coverage_estimate }` | — |
| SecurityAudit | `SecurityAuditInput { code, dependencies, threat_model }` | `SecurityAuditOutput { vulnerabilities, risk_score }` | — |
| DocGen | `DocGenInput { code, doc_type, audience }` | `DocGenOutput { documentation, sections }` | — |
| PptxGen | `PptxGenInput { topic, outline, template }` | `PptxGenOutput { slides, speaker_notes }` | — |
| DeepResearch | `DeepResearchInput { query, sources, depth }` | `DeepResearchOutput { findings, citations, confidence }` | — |
| ImageGen | `ImageGenInput { prompt, style, dimensions }` | `ImageGenOutput { image_prompt, alt_text, metadata }` | — |
| KnowledgeBase | `KnowledgeBaseInput { query, kb_sources }` | `KnowledgeBaseOutput { results, relevance_scores }` | — |
| PlanGen | `PlanGenInput { spec, project_path, context_rules }` | `PlanGenOutput { tasks, estimated_total_turns, summary }` | overrides output_schema() |
| Verification | `VerificationInput { changes, plan_task_id, verification_steps }` | `VerificationOutput { passed, failed_steps, issues, rollback_advice }` | — |
| Superpowers | Skill/Workflow 输入/输出 | skill recommendations + workflow plan | overrides output_schema() |

> **重构机会**: 所有 11 个 plugin 的 `parse_output()` 实现完全相同。参见 [重构 #7](../refactoring/overview.md)。

### parse_utils (pub(crate))

```rust
pub(crate) fn extract_confidence(raw: &str) -> f64;
pub(crate) fn strip_markdown_fence(raw: &str) -> &str;
pub(crate) fn parse_json_from_response(raw: &str) -> Result<serde_json::Value, PluginError>;
```
