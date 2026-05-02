# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.7.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness` + `lattice-agent`（微改）

## 1. 核心设计决定

### 1.1 两套编排模型

**Plugin DAG 是 intra-agent 执行引擎，Pipeline 是 inter-agent 编排器。**
**PluginDagRunner 是 AgentRunner 的平级替代，不嵌入 AgentRunner。**

```
Pipeline::run()
  │
  ├─ profile 无 [plugins] → build_runner() → AgentRunner::run()    (不变)
  │
  └─ profile 有 [plugins] → PluginDagRunner::run()                  (新增)
       │
       ├─ slot "review"  → ErasedPluginRunner (behavior 循环)
       ├─ edge match     → slot "refactor"
       └─ slot "refactor" → ErasedPluginRunner
            └─ no edge   → 返回最终 output
```

### 1.2 累积上下文模型

```
context = {"input": <initial_user_text>}
每个 slot 完成后: context["<slot_name>"] = output
下游 slot 调用: plugin.to_prompt_json(&context)
```

### 1.3 跨 slot 对话状态

每 slot 重建 Agent。跨 slot 硬重置。v0.5.0 §1.2。

### 1.4 Fork 策略

intra-agent DAG 不做 Fork。用 Pipeline Fork。

## 2. 错误类型与传播链

### 2.1 错误分层

```
lattice-core:     LatticeError  (resolve, chat, network, rate limit)
lattice-plugin:   PluginError   (parse, validation, max_turns, escalated)
lattice-harness:  DAGError      (slot not found, plugin not found, resolve, fork rejected)
                  AgentError    (已有，Pipeline 用)
```

### 2.2 传播路径

```
run_plugin_loop()           → Result<RunResult, PluginError>
    ↓ (PluginDagRunner::run 内部转换)
PluginDagRunner::run()      → Result<serde_json::Value, DAGError>
    ↓ (Pipeline::run 内部转换)
Pipeline::run()             → PipelineRun { errors: Vec<AgentError> }
```

### 2.3 PluginError（现有，不变）

```rust
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Validation error: {0}")]
    Validation(String),
    #[error("Missing tool: {0}")]
    MissingTool(String),
    #[error("Context window exceeded: {0} tokens required")]
    ContextExceeded(u32),
    #[error("Max turns exceeded ({0})")]
    MaxTurnsExceeded(u32),
    #[error("Output too large: {0} bytes (max {1})")]
    OutputTooLarge(usize, usize),
    #[error("Escalated after {after_attempts} attempts: {original}")]
    Escalated {
        original: Box<PluginError>,
        after_attempts: u32,
    },
    #[error("{0}")]
    Other(String),
}
```

### 2.4 DAGError（lattice-harness，新增）

```rust
#[derive(Debug, Error)]
pub enum DAGError {
    #[error("entry slot '{0}' not found in [plugins.slots]")]
    EntrySlotNotFound(String),

    #[error("slot '{0}' not found")]
    SlotNotFound(String),

    #[error("plugin '{0}' not registered")]
    PluginNotFound(String),

    #[error("model resolve failed: {0}")]
    Resolve(#[from] lattice_core::LatticeError),

    #[error("max slot transitions ({0}) exceeded — possible infinite DAG loop")]
    MaxSlotTransitionsExceeded(u32),

    #[error("plugin error in slot '{slot}': {source}")]
    Plugin {
        slot: String,
        #[source]
        source: PluginError,
    },

    #[error("output JSON parse failed: {0}")]
    OutputParse(String),

    #[error("fork not supported in intra-agent DAG — use Pipeline")]
    ForkNotSupportedInDag,

    #[error("missing tool registry")]
    MissingToolRegistry,

    #[error("missing plugin registry")]
    MissingPluginRegistry,
}

// PluginDagRunner 内部使用
impl DAGError {
    fn plugin_error(slot: &str, err: PluginError) -> Self {
        DAGError::Plugin { slot: slot.into(), source: err }
    }
}

// Pipeline 错误转换
impl From<DAGError> for AgentError {
    fn from(e: DAGError) -> Self {
        AgentError {
            agent_name: "plugin-dag".into(),
            message: e.to_string(),
            skippable: false,
        }
    }
}
```

## 3. Plugin 层 (lattice-plugin)

### 3.1 Plugin trait（微扩）

```rust
pub trait Plugin: Send + Sync {
    type Input: Serialize + DeserializeOwned + Send;
    type Output: Serialize + DeserializeOwned + Send;

    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    fn to_prompt(&self, input: &Self::Input) -> String;
    fn parse_output(&self, raw: &str) -> Result<Self::Output, PluginError>;
    fn tools(&self) -> &[ToolDefinition] { &[] }
    fn preferred_model(&self) -> &str { "" }
    fn output_schema(&self) -> Option<serde_json::Value> { None }
}
```

**confidence 隐式契约**：如果 Plugin 与 `StrictBehavior` 配合，LLM prompt 中必须要求输出 `"confidence"` 字段（0.0-1.0）。缺失 → 0.0 → StrictBehavior 永远 Retry 直到 max_turns。不需要 confidence 则用 `YoloBehavior`。

### 3.2 ErasedPlugin

```rust
pub trait ErasedPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError>;
    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError>;
    fn tools(&self) -> &[ToolDefinition];
    fn preferred_model(&self) -> &str;
    fn output_schema(&self) -> Option<serde_json::Value>;
}

impl<T: Plugin> ErasedPlugin for T
where T::Input: DeserializeOwned, T::Output: Serialize
{ /* blanket impl — v0.6.0 §3.2 */ }
```

### 3.3 PluginBundle + PluginRegistry

```rust
pub struct PluginBundle {
    pub meta: PluginMeta,
    pub plugin: Box<dyn ErasedPlugin>,
    pub default_behavior: BehaviorMode,
    pub default_tools: Vec<ToolDefinition>,
}

pub enum BehaviorMode {
    Strict { confidence_threshold: f64, max_retries: u32, escalate_to: Option<String> },
    Yolo,
}

impl BehaviorMode {
    pub fn to_behavior(&self) -> Box<dyn Behavior> { /* StrictBehavior / YoloBehavior */ }
}

pub struct PluginRegistry {
    plugins: HashMap<String, PluginBundle>,
}

impl PluginRegistry {
    /// 启动时注册（需 &mut self）。
    /// 完成所有 register 后包装进 Arc，运行时 Arc::get() 只读。
    pub fn register(&mut self, bundle: PluginBundle) -> Result<(), RegistryError>;
    pub fn get(&self, name: &str) -> Option<&PluginBundle>;
}
```

### 3.4 ErasedPluginRunner（共享 run loop）

```rust
// lattice-plugin/src/runner_core.rs

/// 共享的 PluginRunner run loop。
/// 返回 PluginError（不是 DAGError——本 crate 不感知 harness 错误类型）。
pub(crate) fn run_plugin_loop(
    plugin: &dyn ErasedPlugin,
    behavior: &dyn Behavior,
    agent: &mut dyn PluginAgent,
    context: &serde_json::Value,
    config: &PluginConfig,
    hooks: Option<&dyn PluginHooks>,
    retry_policy: Option<&RetryPolicy>,
    memory: Option<&mut dyn Memory>,
) -> Result<RunResult, PluginError> {
    let prompt = plugin.to_prompt_json(context)?;
    let mut attempt = 0u32;

    if let Some(h) = hooks {
        h.on_start(plugin.name(), (prompt.len() as u32).div_ceil(4));
    }

    loop {
        if attempt >= config.max_turns {
            return Err(PluginError::MaxTurnsExceeded(config.max_turns));
        }

        // send_message_with_tools: 内部调 Agent::run()（含 tool loop + 网络重试）
        // 两层重试:
        //   L1 (lattice-core): chat_with_retry — 网络/RateLimit, jittered backoff, 3 次
        //   L2 (这里):         behavior.decide/on_error — 低置信度/解析失败, max_turns 次
        let raw = agent
            .send_message_with_tools(&prompt)
            .map_err(|e| PluginError::Other(e.to_string()))?;

        match plugin.parse_output_json(&raw) {
            Ok(output) => {
                let confidence = extract_confidence(&raw);
                let action = behavior.decide(confidence);

                if let Some(h) = hooks { h.on_turn(attempt, None, &action); }

                match action {
                    Action::Done => {
                        let json = serde_json::to_string(&output)
                            .map_err(|e| PluginError::Other(e.to_string()))?;
                        if json.len() > config.max_output_bytes {
                            return Err(PluginError::OutputTooLarge(json.len(), config.max_output_bytes));
                        }
                        let result = RunResult { output: json, turns: attempt + 1, final_action: Action::Done };
                        if let Some(h) = hooks { h.on_complete(&result); }
                        if let Some(mem) = memory { save_memory_entries(mem, plugin.name(), &prompt, &result); }
                        return Ok(result);
                    }
                    Action::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy { std::thread::sleep(p.jittered_backoff(attempt)); }
                    }
                }
            }
            Err(e) => {
                if let Some(h) = hooks { h.on_error(attempt, &e); }
                match behavior.on_error(&e, attempt) {
                    ErrorAction::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy { std::thread::sleep(p.jittered_backoff(attempt)); }
                    }
                    ErrorAction::Abort => return Err(e),
                    ErrorAction::Escalate => {
                        return Err(PluginError::Escalated {
                            original: Box::new(e),
                            after_attempts: attempt,
                        });
                    }
                }
            }
        }
    }
}
```

### 3.5 PluginAgent 新增方法

```rust
pub trait PluginAgent {
    fn send(&mut self, message: &str) -> Result<String, Box<dyn Error>>;
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>>; // [新增]
    fn set_system_prompt(&mut self, prompt: &str);  // trait: 追加语义
    fn token_usage(&self) -> u64;
}

impl PluginAgent for Agent {
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>> {
        let events = self.run(message, 10);  // Agent::run 含 tool loop + 网络重试
        let mut text = String::new();
        for event in &events {
            if let LoopEvent::Token { text: t } = event { text.push_str(t); }
        }
        Ok(text)
    }
}
```

### 3.6 Agent::set_system_prompt 方法解析

```rust
// inherent method（替换语义）—— PluginDagRunner 用 Agent 直接类型，优先调用此
impl Agent {
    pub fn set_system_prompt(&mut self, prompt: &str) {
        let msg = Message { role: Role::System, content: prompt.to_string(), .. };
        match self.state.messages.first() {
            Some(m) if m.role == Role::System => self.state.messages[0] = msg,
            _ => self.state.messages.insert(0, msg),
        }
    }
}

// trait method（追加语义）—— 通过 &mut dyn PluginAgent 调用时使用
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);
    }
}
```

## 4. AgentProfile 扩展 (lattice-harness)

### 4.1 TOML

```toml
[agent]
name = "code-reviewer"
model = "sonnet"

[plugins]
entry = "review"
shared_tools = ["bash", "read_file", "grep"]

[[plugins.slots]]
name = "review"
plugin = "CodeReview"
max_turns = 3
behavior = { mode = "strict", confidence_threshold = 0.7, max_retries = 3 }

[[plugins.slots]]
name = "refactor"
plugin = "Refactor"
max_turns = 5

[[plugins.edges]]
from = "review"
rule = { condition = { field = "confidence", op = ">", value = "0.7" }, target = "refactor" }

[[plugins.edges]]
from = "review"
rule = { default = true }

[[plugins.edges]]
from = "refactor"
rule = { default = true }

[handoff]
fallback = "deploy-check"
```

### 4.2 Rust 类型

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginsConfig {
    pub entry: String,
    #[serde(default)]
    pub slots: Vec<PluginSlotConfig>,
    #[serde(default)]
    pub edges: Vec<AgentEdgeConfig>,
    #[serde(default)]
    pub shared_tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorMode>,  // 直接存 BehaviorMode
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
}

// TOML 加载中间类型
#[derive(Debug, Clone, Deserialize)]
struct BehaviorModeToml {
    mode: String,
    #[serde(default)]
    confidence_threshold: Option<f64>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    escalate_to: Option<String>,
}

impl TryFrom<BehaviorModeToml> for BehaviorMode {
    type Error = String;
    fn try_from(c: BehaviorModeToml) -> Result<Self, Self::Error> { /* v0.6.0 §6.2 */ }
}

// PluginSlotConfig 自定义 Deserialize
// behavior 字段：BehaviorModeToml → TryFrom → BehaviorMode → Option<BehaviorMode>
```

**entry 校验**（`AgentProfile::load()` 时）：entry 指向已定义 slot → 否则 `LoadError::EntrySlotNotFound`。

## 5. PluginDagRunner (lattice-harness)

### 5.1 常量 + 类型

```rust
/// Slot 最大切换次数。不是 LLM 调用次数，是 slot → slot 转移次数。
/// 与 PluginSlotConfig.max_turns（单 slot 内 LLM 调用上限）正交。
const MAX_DAG_SLOT_TRANSITIONS: u32 = 50;

pub struct PluginDagRunner<'a> {
    config: &'a PluginsConfig,
    plugin_registry: &'a PluginRegistry,
    tool_registry: &'a ToolRegistry,
    retry_policy: RetryPolicy,
    shared_memory: Option<Arc<dyn Memory>>,
}
```

### 5.2 run()

```rust
impl PluginDagRunner<'_> {
    pub fn run(
        &mut self,
        initial_input: &str,
        default_model: &str,
    ) -> Result<serde_json::Value, DAGError> {
        // ── 累积上下文 ──
        // 初始：context = {"input": "<user text>"}
        // Plugin 的 to_prompt_json 接收整个 context。
        // entry plugin 的 Input 应从 context["input"] 获取原始输入。
        let mut context = serde_json::json!({"input": initial_input});

        // entry 校验已在 AgentProfile::load() 完成
        let mut current_name = self.config.entry.clone();
        let mut transitions = 0u32;

        loop {
            if transitions >= MAX_DAG_SLOT_TRANSITIONS {
                return Err(DAGError::MaxSlotTransitionsExceeded(MAX_DAG_SLOT_TRANSITIONS));
            }

            let slot = self.config.slots.iter()
                .find(|s| s.name == current_name)
                .ok_or_else(|| DAGError::SlotNotFound(current_name.clone()))?;

            let bundle = self.plugin_registry.get(&slot.plugin)
                .ok_or_else(|| DAGError::PluginNotFound(slot.plugin.clone()))?;

            // ── 每个 slot 重建 Agent ──
            let model = slot.model_override.as_deref().unwrap_or(default_model);
            let resolved = resolve(model)?;  // LatticeError → DAGError::Resolve
            let mut agent = Agent::new(resolved);
            agent.set_system_prompt(bundle.plugin.system_prompt());  // inherent: 替换

            let tools = merge_tool_definitions(
                &self.tool_registry,
                &self.config.shared_tools,
                &slot.tools,
                bundle.plugin.tools(),
            );
            agent = agent.with_tools(tools);

            // ── ErasedPluginRunner ──
            let behavior = slot.behavior.clone()
                .map(|b| b.to_behavior())
                .unwrap_or_else(|| bundle.default_behavior.clone().to_behavior());

            let plugin_config = PluginConfig {
                max_turns: slot.max_turns.unwrap_or(10),
                ..Default::default()
            };

            let result = ErasedPluginRunner::run_with(
                bundle.plugin.as_ref(),
                behavior.as_ref(),
                &mut agent,
                &context,
                &plugin_config,
                Some(&self.retry_policy),
                self.shared_memory.as_deref(),
            )
            .map_err(|e| DAGError::plugin_error(&current_name, e))?;

            let output_json: serde_json::Value = serde_json::from_str(&result.output)
                .map_err(|e| DAGError::OutputParse(e.to_string()))?;

            // ── 写入累积上下文 ──
            context[current_name.as_str()] = output_json.clone();

            // ── 保存到 shared_memory ──
            if let Some(ref mem) = self.shared_memory {
                mem.save_entry(MemoryEntry {
                    id: format!("dag-{}-{}", current_name, transitions),
                    kind: EntryKind::SessionLog,
                    session_id: self.config.entry.clone(),
                    summary: format!("{} output", current_name),
                    content: result.output,
                    tags: vec![current_name.clone()],
                    created_at: now_ms().to_string(),
                });
            }

            // ── 找下一个 slot（edges 按 TOML 定义顺序求值，first match wins）──
            let next = self.find_edge(&current_name, &output_json);

            match next {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => return Err(DAGError::ForkNotSupportedInDag),
                None => return Ok(output_json),
            }
        }
    }

    /// 沿 edges 的 TOML 定义顺序，找第一条 from == current && rule.eval(output) == true 的边。
    /// 返回 rule.target。无匹配 → None（DAG 终点）。
    fn find_edge(&self, from: &str, output: &serde_json::Value) -> Option<HandoffTarget> {
        self.config.edges.iter()
            .filter(|e| e.from == from)
            .find(|e| e.rule.eval(output))
            .and_then(|e| e.rule.target.clone())
    }
}
```

### 5.3 初始 input → entry plugin Input 映射

Entry plugin 通过 `to_prompt_json(&context)` 接收初始上下文。context 结构：

```json
{"input": "<用户原始文本>"}
```

Plugin Input 需有对应字段（`#[serde(default)]` 确保缺失不报错）：

```rust
// CodeReviewInput — entry plugin 示例
#[derive(Deserialize)]
struct CodeReviewInput {
    #[serde(default)]
    input: String,         // ← context["input"] → 用户原始文本（当 diff 用）
    #[serde(default)]
    file_path: String,
    #[serde(default)]
    context_rules: Vec<String>,
}
// context["input"] = "sort this Rust code..." → CodeReviewInput.input = "sort this..."
// CodeReview::to_prompt(): self.input 作为 diff 传给 LLM
```

非 entry plugin 的 Input 可以同时引用 `context["input"]` 和 `context["<上游slot>"]`：

```rust
#[derive(Deserialize)]
struct RefactorInput {
    #[serde(default)]
    code: String,                  // ← context["input"]
    #[serde(default)]
    review_issues: Vec<Issue>,    // ← context["review"]["issues"]
    #[serde(default)]
    instructions: String,
}
```

## 6. ToolRegistry (lattice-harness)

```rust
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

pub fn merge_tool_definitions(
    registry: &ToolRegistry,
    shared_tool_names: &[String],
    slot_tool_names: &[String],
    plugin_tools: &[ToolDefinition],
) -> Vec<ToolDefinition> {
    // 逐层合并。同名：plugin > slot > shared
    // tool 名不在 registry → warn! + skip
    // v0.6.0 §8
}
```

## 7. Pipeline 集成

**PluginDagRunner 是 AgentRunner 的平级替代，在 Pipeline 中分支**。

```rust
impl Pipeline {
    pub fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun {
        // ... 前面不变 ...
        for _turn in 0..pipeline_max_agents {
            let profile = self.registry.get(&current_agent)?;

            let resolved = match resolve(&profile.agent.model) {
                Ok(r) => r,
                Err(e) => { /* 现有错误处理 */ }
            };

            // ── 分支：Plugin DAG vs 原始 Agent ──
            let output: serde_json::Value = if let Some(ref plugins_config) = profile.plugins {
                let plugin_registry = self.plugin_registry.as_ref()
                    .ok_or_else(|| AgentError { ... })?;
                let tool_registry = self.tool_registry.as_ref()
                    .ok_or_else(|| AgentError { ... })?;

                let mut dag = PluginDagRunner::new(
                    plugins_config,
                    plugin_registry,
                    tool_registry,
                );
                dag.run(&current_input, &profile.agent.model)
                    .map_err(|e| AgentError::from(e))?
            } else {
                let mut runner = build_runner(&profile, resolved, self.shared_memory.clone());
                runner.run(&current_input, agent_max_turns)
                    .map_err(|e: Box<dyn Error>| AgentError { ... })?
            };

            // ── handoff 评估（两种模式共用）──
            let next = eval_rules_or_fallback(&profile, &output);
            // ... 后续不变 ...
        }
    }
}

// Pipeline 新增字段
impl Pipeline {
    pub plugin_registry: Option<Arc<PluginRegistry>>,
    pub tool_registry: Option<Arc<ToolRegistry>>,
}
```

**AgentRunner 不变**——Plugin 模式绕开 AgentRunner，直接从 Pipeline 调 PluginDagRunner。`build_runner()` 仅在非 plugin 模式调用。

## 8. 初始化模式

```rust
// 启动时
let mut plugin_registry = PluginRegistry::new();
plugin_registry.register(code_review_bundle)?;
plugin_registry.register(refactor_bundle)?;
// ... 注册所有插件 ...

let plugin_registry = Arc::new(plugin_registry);  // 冻结，之后只读

let mut tool_registry = ToolRegistry::new();
// 注册工具 ...

let tool_registry = Arc::new(tool_registry);

let pipeline = Pipeline::new("my-pipeline", agent_registry, shared_memory, event_bus)
    .with_plugin_registry(plugin_registry)
    .with_tool_registry(tool_registry);

pipeline.run("code-reviewer", "sort this Rust code");
```

## 9. 内置插件

| 插件 | Input struct（#[serde(default)] 所有字段） | 文件 |
|------|------------------------------------------|------|
| `CodeReview` | `{ input, file_path, context_rules }` | builtin/code_review.rs |
| `Refactor` | `{ code, review_issues, instructions }` | builtin/refactor.rs |
| `TestGen` | `{ code, focus_areas }` | builtin/test_gen.rs |
| `SecurityAudit` | `{ code, dependencies, threat_model }` | builtin/security_audit.rs |
| `DocGen` | `{ code, doc_type, audience }` | builtin/doc_gen.rs |
| `PptxGen` | `{ topic, outline, template }` | builtin/pptx_gen.rs |
| `DeepResearch` | `{ query, sources, depth }` | builtin/deep_research.rs |
| `ImageGen` | `{ prompt, style, dimensions }` | builtin/image_gen.rs |
| `KnowledgeBase` | `{ query, kb_sources }` | builtin/knowledge_base.rs |

所有 Input struct 字段标记 `#[serde(default)]`。共用 `builtin/parse_utils.rs`。

## 10. 集成测试

```rust
#[test]
fn test_dag_review_then_refactor_context_accumulation() {
    let mut pr = PluginRegistry::new();
    pr.register(/* CodeReview */);
    pr.register(/* Refactor */);
    let pr = Arc::new(pr);
    let tr = Arc::new(ToolRegistry::new());

    let config = PluginsConfig {
        entry: "review".into(),
        shared_tools: vec![],
        slots: vec![
            PluginSlotConfig { name: "review".into(), plugin: "CodeReview".into(), max_turns: Some(3), .. },
            PluginSlotConfig { name: "refactor".into(), plugin: "Refactor".into(), max_turns: Some(5), .. },
        ],
        edges: vec![
            AgentEdgeConfig { from: "review".into(), rule: HandoffRule { default: true, target: Some("refactor".into()), .. } },
            AgentEdgeConfig { from: "refactor".into(), rule: HandoffRule { default: true, .. } },
        ],
    };

    let mut dag = PluginDagRunner::new(&config, &pr, &tr);
    let result = dag.run("// broken code", "mock-model").unwrap();
    assert!(result.get("refactored_code").is_some());
}

#[test]
fn test_entry_slot_not_found() { /* AgentProfile::load → Err */ }
#[test]
fn test_missing_tool_warned() { /* tool 不在 registry → warn + skip，不 panic */ }
#[test]
fn test_fork_rejected_in_dag() { /* Fork Target → DAGError::ForkNotSupportedInDag */ }
#[test]
fn test_max_transitions_exceeded() { /* 循环 edge → DAGError::MaxSlotTransitionsExceeded */ }
```

## 11. 实现顺序

```
第1轮:  ErasedPlugin trait + blanket impl
第2轮:  PluginBundle + PluginMeta + BehaviorMode + to_behavior()
第3轮:  PluginRegistry（构建期注册 + Arc 冻结）
第4轮:  Agent::set_system_prompt() inherent（替换语义）
第5轮:  PluginAgent::send_message_with_tools()
第6轮:  PluginRunner 重构 → ErasedPluginRunner + 共享 run_plugin_loop()
          run_plugin_loop 返回 PluginError（不感知 DAGError）
第7轮:  PluginsConfig + PluginSlotConfig + AgentEdgeConfig + BehaviorModeToml → TryFrom
第8轮:  DAGError（完整 10 variant + From<LatticeError> + Into<AgentError>）
第9轮:  PluginDagRunner + 累积上下文 + find_edge + shared_memory
第10轮: Pipeline 集成（profile.plugins 分支，PluginDagRunner 平级）
第11轮: ToolRegistry + merge_tool_definitions
第12轮: parse_utils + 3 个内置插件（全部 Input 字段 #[serde(default)]）
第13轮: 剩余 6 个内置插件
第14轮: 集成测试
```

## 12. 与现有代码的关系

### 不动
- `lattice-core`: 全部
- `lattice-agent`: AgentState, Memory trait, 7 内置工具, PluginAgent trait 现有方法签名
- `lattice-harness`: AgentRunner, AgentProfile（无 plugin 字段的加载逻辑）, HandoffRule, HandoffTarget, HandoffCondition, eval_rules, AgentRegistry, EventBus, Watcher, WebSocket, dry_run, handle_fallback
- `lattice-plugin`: Behavior trait, StrictBehavior, YoloBehavior, PluginHooks, PluginConfig, RunResult, PluginError, Action, ErrorAction, extract_confidence（改为 pub(crate)）

### 修改
- `lattice-plugin/src/lib.rs`: Plugin trait 加 `output_schema()`；`extract_confidence` → `pub(crate)`；PluginRunner::run 重构为委托 run_plugin_loop
- `lattice-agent/src/agent.rs`: 加 inherent `set_system_prompt()`
- `lattice-agent/src/lib.rs`: PluginAgent trait 加 `send_message_with_tools()` + Agent impl
- `lattice-harness/src/profile.rs`: AgentProfile 加 `plugins: Option<PluginsConfig>`；PluginsConfig / PluginSlotConfig / AgentEdgeConfig / BehaviorModeToml + TryFrom + 自定义 Deserialize
- `lattice-harness/src/pipeline.rs`: Pipeline 加 plugin_registry/tool_registry 字段；run() 分支：plugin 模式 → PluginDagRunner，否则 → build_runner + AgentRunner
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`

### 新增
- `lattice-plugin/src/erased.rs`
- `lattice-plugin/src/registry.rs`
- `lattice-plugin/src/bundle.rs`
- `lattice-plugin/src/erased_runner.rs`
- `lattice-plugin/src/builtin/`（9 插件 + parse_utils）
- `lattice-harness/src/dag_runner.rs`（PluginDagRunner + DAGError）
- `lattice-harness/src/tools.rs`

## 13. 不在此 spec

- Swarm 去中心化编排
- Python 胶水层 + pip 分发
- 运行时动态加载
- intra-agent Fork
- Agent 工具执行层重构
