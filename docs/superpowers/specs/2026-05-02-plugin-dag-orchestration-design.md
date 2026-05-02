# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.6.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness` + `lattice-agent`（微改）

## 1. 核心设计决定

### 1.1 两套编排模型

**Plugin DAG 是 intra-agent 执行引擎，Pipeline 是 inter-agent 编排器。**

### 1.2 跨 slot 数据流：累积上下文模型

每个 slot 的 output 写入累积 context。下游 slot 看到**所有上游 output + 初始 input**：

```
context = {"input": <initial_user_text>}

slot "review" → 看到 context
  → to_prompt_json(context)     // Plugin 从 context 提取需要的字段
  → 产出 {"issues": [...], "confidence": 0.85}
  → context["review"] = output  // 写入累积

slot "refactor" → 看到 context = {
    "input": "original user code...",
    "review": {"issues": [...], "confidence": 0.85}
  }
  → to_prompt_json(context)     // 能看到原始 code + review 结果
  → 产出 {"refactored_code": "...", "changes": [...]}
  → context["refactor"] = output
```

**为什么不用"上游 Output = 下游 Input"**：每个 Plugin 的 Input 类型不同。CodeReview 产出 `{issues, confidence}`，Refactor 期望 `{code, review_issues, instructions}`——结构完全不匹配。累积 context 让每个 Plugin 自行从完整上下文中提取所需字段。

**Plugin 作者的责任**：编写 `to_prompt_json(context)` 时从 context 中提取需要的字段（`context["input"]` 是原始文本，`context["<slot_name>"]` 是各 slot 输出）。

### 1.3 跨 slot 对话状态

每个 slot 开始时重建 `lattice_agent::Agent`。跨 slot 边界**硬重置**（设计决定，见 v0.5.0 1.2 节）。

### 1.4 Fork 策略

intra-agent DAG 不做 Fork。Pipeline 已有完整 Fork 支持。

## 2. 命名约定

| Spec 名称 | 实际 Rust 类型 | 说明 |
|-----------|---------------|------|
| `Agent` | `lattice_agent::Agent` | agent crate 的 Agent struct。Spec 中直接称 `Agent`，不称 `LlmAgent` |
| `PluginAgent` | `lattice_agent::PluginAgent` | trait，Agent 实现它 |
| `DagAgent` | 无 | Spec 不使用 DagAgent。编排逻辑在 `PluginDagRunner` |

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
    fn output_schema(&self) -> Option<serde_json::Value> { None }  // [新增]
}
```

**confidence 隐式契约**：如果 Plugin 与 `StrictBehavior` 配合使用，LLM prompt 中必须要求输出 `"confidence"` 字段（0.0-1.0）。`extract_confidence` 在缺失时返回 0.0 → StrictBehavior 会无限 Retry 直到 max_turns 耗尽。如果不想输出 confidence，用 `YoloBehavior`。

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
where
    T::Input: DeserializeOwned,
    T::Output: Serialize,
{
    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError> {
        let typed: T::Input = serde_json::from_value(context.clone())
            .map_err(|e| PluginError::Parse(format!(
                "{}: failed to deserialize input from context: {}", self.name(), e
            )))?;
        Ok(self.to_prompt(&typed))
    }
    // parse_output_json, name, system_prompt, tools, preferred_model, output_schema 同上
}
```

**关键变更**：`to_prompt_json` 的参数名从 `input` 改为 `context`。接收的是累积 context（含 input + 所有上游 slot 输出），不是单一 slot 的输出。Plugin 的 `Input` 类型应设计为能从累积 context 中 deserialize 出所需字段（用 `#[serde(default)]` 处理缺失字段）。

### 3.3 BehaviorMode（含转换）

```rust
pub enum BehaviorMode {
    Strict { confidence_threshold: f64, max_retries: u32, escalate_to: Option<String> },
    Yolo,
}

impl BehaviorMode {
    pub fn to_behavior(&self) -> Box<dyn Behavior> { /* 已有 */ }
}

// ---- TOML 反序列化 → BehaviorMode 的转换 ----
// BehaviorModeConfig 在 AgentProfile::load() 时转换为 BehaviorMode
// PluginSlotConfig 直接存 BehaviorMode（不是 BehaviorModeConfig）

pub fn resolve_behavior(
    slot: &PluginSlotConfig,
    default: &BehaviorMode,
) -> Box<dyn Behavior> {
    match &slot.behavior {
        Some(bm) => bm.clone().to_behavior(),
        None => default.clone().to_behavior(),
    }
}
```

**类型一致性**：`PluginSlotConfig.behavior: Option<BehaviorMode>`（不是 `BehaviorModeConfig`）。TOML 反序列化时 `BehaviorModeConfig` 通过 `try_from` 转换为 `BehaviorMode`。

## 4. PluginBundle + PluginRegistry

```rust
pub struct PluginBundle {
    pub meta: PluginMeta,
    pub plugin: Box<dyn ErasedPlugin>,
    pub default_behavior: BehaviorMode,
    pub default_tools: Vec<ToolDefinition>,
}

pub struct PluginRegistry {
    plugins: HashMap<String, PluginBundle>,
}

impl PluginRegistry {
    /// 运行时只读查询
    pub fn get(&self, name: &str) -> Option<&PluginBundle>;

    /// 启动时注册（需 &mut self）。
    /// 构建期完成所有 register 后，包装进 Arc 供 AgentRunner 共享。
    pub fn register(&mut self, bundle: PluginBundle) -> Result<(), RegistryError>;
}

// 使用模式：
// let mut registry = PluginRegistry::new();
// registry.register(code_review_bundle);
// registry.register(refactor_bundle);
// let registry = Arc::new(registry);  // 之后只读共享
```

## 5. ErasedPluginRunner (lattice-plugin)

### 5.1 共享 run loop

```rust
// lattice-plugin/src/runner_core.rs

pub(crate) fn run_plugin_loop(
    plugin: &dyn ErasedPlugin,
    behavior: &dyn Behavior,
    agent: &mut dyn PluginAgent,
    context: &serde_json::Value,           // 累积上下文
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
                        if let Some(mem) = memory {
                            save_memory_entries(mem, plugin.name(), &prompt, &result);
                        }
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
                        return Err(PluginError::Escalated { original: Box::new(e), after_attempts: attempt });
                    }
                }
            }
        }
    }
}
```

### 5.2 PluginAgent 新增方法

```rust
pub trait PluginAgent {
    fn send(&mut self, message: &str) -> Result<String, Box<dyn Error>>;
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>>; // [新增]
    fn set_system_prompt(&mut self, prompt: &str);  // trait: push 语义（追加）
    fn token_usage(&self) -> u64;
}

impl PluginAgent for Agent {
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>> {
        let events = self.run(message, 10);
        let mut text = String::new();
        for event in &events {
            if let LoopEvent::Token { text: t } = event { text.push_str(t); }
        }
        Ok(text)
    }
}
```

### 5.3 Agent::set_system_prompt 的 inherent vs trait 方法

```rust
// trait 方法（追加语义，现有行为不改）
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);  // 追加
    }
}

// inherent 方法（替换语义，新增）
impl Agent {
    /// 替换已有的 system message（非追加）。
    /// 当通过 Agent 直接调用时（非 &mut dyn PluginAgent），此 inherent 方法优先。
    pub fn set_system_prompt(&mut self, prompt: &str) {
        let system_msg = Message { role: Role::System, content: prompt.to_string(), .. };
        match self.state.messages.first() {
            Some(msg) if msg.role == Role::System => self.state.messages[0] = system_msg,
            _ => self.state.messages.insert(0, system_msg),
        }
    }
}

// 调用行为：
// let mut a: Agent = ...;  a.set_system_prompt("x");  → inherent (替换) ✓
// let mut a: &mut dyn PluginAgent = ...;  a.set_system_prompt("x");  → trait (追加)
// PluginDagRunner 使用 Agent 直接类型 → 调用 inherent → 替换语义 ✓
```

## 6. AgentProfile 扩展 (lattice-harness)

### 6.1 TOML

```toml
[agent]
name = "code-reviewer"
model = "sonnet"

[plugins]
entry = "review"
shared_tools = ["bash", "read_file", "grep", "web_search"]

[[plugins.slots]]
name = "review"
plugin = "CodeReview"
max_turns = 3
tools = ["diff_parser"]                       # slot 专属工具（加入 shared）
behavior = { mode = "strict", confidence_threshold = 0.7, max_retries = 3 }

[[plugins.slots]]
name = "refactor"
plugin = "Refactor"
max_turns = 5
model_override = "sonnet"

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

### 6.2 Rust 类型

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
    pub behavior: Option<BehaviorMode>,  // BehaviorMode，不是 BehaviorModeConfig
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
}

// TOML 反序列化的中间类型
#[derive(Debug, Clone, Deserialize)]
struct BehaviorModeConfig {
    mode: String,
    #[serde(default)]
    confidence_threshold: Option<f64>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    escalate_to: Option<String>,
}

impl TryFrom<BehaviorModeConfig> for BehaviorMode {
    type Error = String;
    fn try_from(c: BehaviorModeConfig) -> Result<Self, Self::Error> {
        match c.mode.as_str() {
            "yolo" => Ok(BehaviorMode::Yolo),
            "strict" => Ok(BehaviorMode::Strict {
                confidence_threshold: c.confidence_threshold.unwrap_or(0.7),
                max_retries: c.max_retries.unwrap_or(3),
                escalate_to: c.escalate_to,
            }),
            other => Err(format!("unknown behavior mode '{}'", other)),
        }
    }
}

// PluginSlotConfig 的 TOML 反序列化手工实现
// behavior 字段：先 deserialize 为 Option<BehaviorModeConfig>，再 try_into → Option<BehaviorMode>
impl<'de> Deserialize<'de> for PluginSlotConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: Deserializer<'de> {
        // 使用 RawSlotConfig 辅助结构，behavior 存 Option<BehaviorModeConfig>
        // 然后转换为 PluginSlotConfig { behavior: raw.behavior.map(|c| c.try_into()).transpose()? }
        ...
    }
}
```

**entry 校验**（`AgentProfile::load()` 时）：
1. `entry` 指向已定义的 slot → 否则 `LoadError::EntrySlotNotFound`
2. `edges` 的所有 `from` 指向已定义的 slot → warn
3. 每个 node 存在可达的终止路径 → warn

## 7. PluginDagRunner (lattice-harness)

### 7.1 常量

```rust
/// slot 最大切换次数。防止死循环。
const MAX_DAG_SLOT_TRANSITIONS: u32 = 50;
```

### 7.2 DAGError

```rust
#[derive(Debug, Error)]
pub enum DAGError {
    #[error("entry slot '{0}' not found")]
    EntrySlotNotFound(String),
    #[error("slot '{0}' not found")]
    SlotNotFound(String),
    #[error("plugin '{0}' not registered")]
    PluginNotFound(String),
    #[error("model resolve failed: {0}")]
    Resolve(String),
    #[error("max slot transitions ({0}) exceeded")]
    MaxSlotTransitionsExceeded(u32),
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error("output parse failed: {0}")]
    Parse(String),
    #[error("fork not supported in intra-agent DAG")]
    ForkNotSupportedInDag,
    #[error("plugin '{plugin}' escalated after {after_attempts} attempts: {original}")]
    Escalated { plugin: String, after_attempts: u32, original: String },
}
```

### 7.3 run()

```rust
pub struct PluginDagRunner<'a> {
    config: &'a PluginsConfig,
    plugin_registry: &'a PluginRegistry,
    tool_registry: &'a ToolRegistry,
    retry_policy: RetryPolicy,
    shared_memory: Option<Arc<dyn Memory>>,
}

impl PluginDagRunner<'_> {
    pub fn run(
        &mut self,
        initial_input: &str,
        default_model: &str,
    ) -> Result<serde_json::Value, DAGError> {
        // ── 累积上下文 ──
        let mut context = serde_json::Map::new();
        context.insert("input".into(), serde_json::Value::String(initial_input.to_string()));
        let mut context = serde_json::Value::Object(context);

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
            let resolved = resolve(model).map_err(|e| DAGError::Resolve(e.to_string()))?;
            let mut agent = Agent::new(resolved);
            agent.set_system_prompt(bundle.plugin.system_prompt());  // inherent: 替换

            let tools = merge_tool_definitions(
                &self.tool_registry,
                &self.config.shared_tools,   // plugins.shared_tools
                &slot.tools,                  // slot.tools
                bundle.plugin.tools(),        // plugin.tools()
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
                &context,                     // ★ 传入累积上下文
                &plugin_config,
                Some(&self.retry_policy),
                self.shared_memory.as_deref(),
            )?;

            let output_json: serde_json::Value = serde_json::from_str(&result.output)
                .map_err(|e| DAGError::Parse(e.to_string()))?;

            // ── 写入累积上下文 ──
            context[current_name.clone()] = output_json.clone();

            // ── 保存到 shared_memory ──
            if let Some(ref mem) = self.shared_memory {
                let entry = MemoryEntry {
                    id: format!("dag-{}-{}", current_name, transitions),
                    kind: EntryKind::SessionLog,
                    session_id: self.config.entry.clone(),
                    summary: format!("{} slot output", current_name),
                    content: result.output.clone(),
                    tags: vec![current_name.clone()],
                    created_at: now_ms().to_string(),
                };
                mem.save_entry(entry);
            }

            // ── 找下一个 slot ──
            let next = self.find_edge(&current_name, &output_json);

            match next {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => {
                    return Err(DAGError::ForkNotSupportedInDag);
                }
                None => return Ok(output_json),
                // ★ 最终输出是最后一个 slot 的 output。
                // DAG 沿第一条匹配的 edge 走（HandoffRule eval 顺序），所以路径是确定性的。
            }
        }
    }

    fn find_edge(&self, from: &str, output: &serde_json::Value) -> Option<HandoffTarget> {
        self.config.edges.iter()
            .filter(|e| e.from == from)
            .find(|e| e.rule.eval(output))
            .and_then(|e| e.rule.target.clone())
    }
}
```

### 7.4 数据流示意

```
context = {"input": "sort this Rust code: fn main() { ... }"}

slot "review" 看到 context →
  CodeReview::to_prompt_json(context)
    → serde_json::from_value::<ReviewInput>(context) → ReviewInput { code: "...", file_path: "", context_rules: "" }
    → prompt = "Review this code: ..."
    → parse_output → {"issues": [{...}], "confidence": 0.85}
  context["review"] = {"issues": [{...}], "confidence": 0.85}

slot "refactor" 看到 context = {
    "input": "sort this Rust code: ...",
    "review": {"issues": [...], "confidence": 0.85}
  }
  → Refactor::to_prompt_json(context)
    → serde_json::from_value::<RefactorInput>(context) → RefactorInput {
        code: context["input"].as_str(),                // 从 input 拿原始代码
        review_issues: context["review"]["issues"],      // 从 review 拿审查结果
        instructions: ""                                 // 可选字段，默认空
      }
    → prompt = "Refactor this code fixing: [...issues...]"
    → parse_output → {"refactored_code": "...", "changes": [...]}
  context["refactor"] = {...}
```

## 8. ToolRegistry (lattice-harness)

```rust
/// 合并三层工具定义。
/// - shared: 从 ToolRegistry 解析 plugins.shared_tools
/// - slot: 从 ToolRegistry 解析 slot.tools
/// - plugin: plugin.tools() 返回
/// 同名：plugin > slot > shared。tool 名不在 registry → warn + skip。
pub fn merge_tool_definitions(
    registry: &ToolRegistry,
    shared_tool_names: &[String],
    slot_tool_names: &[String],
    plugin_tools: &[ToolDefinition],
) -> Vec<ToolDefinition> {
    let mut merged: IndexMap<String, ToolDefinition> = IndexMap::new();

    for names in [shared_tool_names, slot_tool_names] {
        for name in names {
            match registry.get(name) {
                Some(tool) => { merged.insert(name.clone(), tool.definition.clone()); }
                None => warn!("tool '{}' not in ToolRegistry — skipping", name),
            }
        }
    }

    for td in plugin_tools {
        merged.insert(td.function.name.clone(), td.clone());
    }

    merged.into_values().collect()
}
```

## 9. AgentRunner 集成

```rust
impl AgentRunner {
    // 新增字段
    pub plugin_registry: Option<Arc<PluginRegistry>>,   // Arc: 只读共享
    pub tool_registry: Option<Arc<ToolRegistry>>,

    pub fn run(&mut self, input: &str, max_turns: u32) -> Result<serde_json::Value, Box<dyn Error>> {
        if let Some(ref plugins_config) = self.profile.plugins {
            let registry = self.plugin_registry.as_ref().ok_or("plugin_registry not set")?;
            let tools = self.tool_registry.as_ref().ok_or("tool_registry not set")?;
            let mut dag = PluginDagRunner::new(plugins_config, registry, tools);
            let output = dag.run(input, &self.profile.agent.model)?;
            self.validate_with_schema(output, max_turns)
        } else {
            self.run_raw(input, max_turns)  // 不变
        }
    }
}
```

## 10. 内置插件

| 插件 | Input struct | 从 context 提取的关键字段 | 文件 |
|------|-------------|--------------------------|------|
| `CodeReview` | `CodeReviewInput` | `context["input"]` → code, `context["<prev>"]["diff"]` → diff | builtin/code_review.rs |
| `Refactor` | `RefactorInput` | `context["input"]` → code, `context["review"]["issues"]` → issues | builtin/refactor.rs |
| `TestGen` | `TestGenInput` | `context["input"]` → code, `context["refactor"]["refactored_code"]` → code | builtin/test_gen.rs |
| `SecurityAudit` | `SecurityAuditInput` | `context["input"]` → code, context 中依赖列表 | builtin/security_audit.rs |
| `DocGen` | `DocGenInput` | `context["refactor"]["refactored_code"]` → code | builtin/doc_gen.rs |
| `PptxGen` | `PptxGenInput` | context 中 topic, outline | builtin/pptx_gen.rs |
| `DeepResearch` | `DeepResearchInput` | context 中 query, sources | builtin/deep_research.rs |
| `ImageGen` | `ImageGenInput` | context 中 prompt, style | builtin/image_gen.rs |
| `KnowledgeBase` | `KnowledgeBaseInput` | context 中 query, sources | builtin/knowledge_base.rs |

每个 Plugin 的 `Input` struct 所有字段标记 `#[serde(default)]`，确保从 context 反序列化时缺失字段不会导致 Parse error。

共享工具：`builtin/parse_utils.rs`。

## 11. 集成测试场景

```rust
#[test]
fn test_dag_review_then_refactor_with_context_accumulation() {
    let mut registry = PluginRegistry::new();
    registry.register(/* CodeReview bundle */);
    registry.register(/* Refactor bundle */);
    let registry = Arc::new(registry);  // 之后只读

    let config = PluginsConfig {
        entry: "review".into(),
        shared_tools: vec![],
        slots: vec![
            PluginSlotConfig { name: "review".into(), plugin: "CodeReview".into(), .. },
            PluginSlotConfig { name: "refactor".into(), plugin: "Refactor".into(), .. },
        ],
        edges: vec![
            AgentEdgeConfig { from: "review".into(), rule: HandoffRule { default: true, target: Some("refactor".into()), .. } },
            AgentEdgeConfig { from: "refactor".into(), rule: HandoffRule { default: true, .. } },
        ],
    };

    let tool_registry = ToolRegistry::new();
    let mut dag = PluginDagRunner::new(&config, &registry, &tool_registry);
    let result = dag.run("// broken code", "mock-model").unwrap();

    // refactor 的 output 是最终输出（最后一个 slot）
    assert!(result.get("refactored_code").is_some());
}

#[test]
fn test_entry_slot_not_found() { /* AgentProfile::load 报错 */ }

#[test]
fn test_missing_tool_warned_not_error() { /* ToolRegistry 缺失 → warn + skip */ }

#[test]
fn test_behavior_mode_config_to_enum_conversion() {
    // "strict" 含 escalate_to → Strict { confidence_threshold, max_retries, escalate_to }
    // "yolo" → Yolo
    // "unknown" → Err
}
```

## 12. 实现顺序

```
第1轮:  ErasedPlugin trait + blanket impl（to_prompt_json 参数名 context）
第2轮:  PluginBundle + PluginMeta + BehaviorMode
第3轮:  PluginRegistry（构建期注册 + Arc 只读共享）
第4轮:  Agent::set_system_prompt() inherent + trait 语义文档
第5轮:  PluginAgent::send_message_with_tools()
第6轮:  PluginRunner 重构 → ErasedPluginRunner + 共享 run_plugin_loop()
第7轮:  PluginsConfig + PluginSlotConfig + AgentEdgeConfig（手工 Deserialize，BehaviorMode 直接存）
第8轮:  PluginDagRunner + DAGError + 累积上下文 + find_edge + shared_memory 保存
第9轮:  AgentRunner 集成
第10轮: ToolRegistry + merge_tool_definitions（shared + slot + plugin 合并）
第11轮: parse_utils + 3 个内置插件（Input 全字段 #[serde(default)]）
第12轮: 剩余 6 个内置插件
第13轮: 集成测试
```

## 13. 与现有代码的关系

### 不动
- `lattice-core`: 全部
- `lattice-agent`: AgentState, Memory trait, 7 个内置工具, PluginAgent trait 的现有方法签名
- `lattice-harness`: Pipeline, HandoffRule, HandoffTarget, HandoffCondition, eval_rules, AgentRegistry, EventBus, Watcher, WebSocket, dry_run
- `lattice-plugin`: Behavior trait, StrictBehavior, YoloBehavior, PluginHooks, PluginConfig, RunResult, PluginError, Action, ErrorAction

### 修改
- `lattice-plugin/src/lib.rs`: Plugin trait 加 `output_schema()`；`extract_confidence` → `pub(crate)`
- `lattice-plugin/src/runner.rs`: 重构为共享 `run_plugin_loop()`
- `lattice-agent/src/agent.rs`: 加 inherent `set_system_prompt()` 替换语义
- `lattice-agent/src/lib.rs`: PluginAgent trait 加 `send_message_with_tools()` + Agent impl
- `lattice-harness/src/profile.rs`: 加 PluginsConfig 等类型 + BehaviorModeConfig → BehaviorMode TryFrom + PluginSlotConfig 自定义 Deserialize
- `lattice-harness/src/runner.rs`: AgentRunner 加 plugin_registry/tool_registry 字段 + run() 分支
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`

### 新增
- `lattice-plugin/src/erased.rs`
- `lattice-plugin/src/registry.rs`
- `lattice-plugin/src/bundle.rs`
- `lattice-plugin/src/erased_runner.rs`
- `lattice-plugin/src/builtin/`（9 插件 + parse_utils）
- `lattice-harness/src/dag_runner.rs`
- `lattice-harness/src/tools.rs`

## 14. 不在此 spec

- Swarm 去中心化编排
- Python 胶水层 + pip 分发
- 运行时动态加载
- intra-agent Fork
- Agent 工具执行层重构
