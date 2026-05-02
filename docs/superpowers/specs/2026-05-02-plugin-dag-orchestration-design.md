# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.5.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness` + `lattice-agent`（微改）

## 1. 核心设计决定

### 1.1 两套编排模型的关系

**Plugin DAG 是 intra-agent 执行引擎，Pipeline 是 inter-agent 编排器。**

```
Pipeline (不变)
  │
  ├─ Agent A ("code-review")
  │     └─ 内部: Plugin DAG → PluginDagRunner → ErasedPluginRunner (per slot)
  │
  ├─ handoff eval_rules → Agent B ("deploy-check")
  │     └─ 内部: 简单模式 → AgentRunner.run_raw() (不变)
  │
  └─ handoff eval_rules → None (结束)
```

- **Pipeline**：唯一 inter-agent 编排入口。不变。
- **PluginDagRunner**：新增。当 AgentProfile 有 `[plugins]` 段时，AgentRunner 委托给它。
- **ErasedPluginRunner**：PluginRunner 的类型擦除版。

### 1.2 跨 slot 状态模型（明确设计决定）

每个 slot 开始时重建 `LlmAgent`。跨 slot 边界**硬重置**：

```
slot A (review):  LlmAgent { messages: [system, user, assistant, user, assistant, ...] }
  → 产出 JSON output
  → slot A 的 Agent 销毁

slot B (refactor): LlmAgent::new()  // 全新，只有 system prompt
  → to_prompt_json(slot_A_output)   // JSON 作为结构化 input，不是对话
  → push_user_message(prompt)       // 冷启动
```

**为什么**：Plugin 的 contract 是 Input → Output，不是对话连续性。跨 slot 信息传递走 JSON（结构化、可校验），不走非结构化的 chat history。review 产出的 `{issues: [...], confidence: 0.9}` 是 refactor 的类型化 Input，refactor 不需要知道 review 当时聊了什么。

**slot 内部**：同一 slot 的 retry 保留 messages 历史（同一 Agent 不重建），behavior.decide 走 Retry 时 LLM 看到"上次不够好"的上下文。

### 1.3 Fork 策略

intra-agent DAG 不做 Fork。并行编排用 Pipeline Fork（`target = "fork:A,B"`）。

## 2. 架构分层

```
lattice-harness
  Pipeline              不变  inter-agent 编排
  AgentRunner            扩展  检测 [plugins] → PluginDagRunner
  PluginDagRunner        新增  intra-agent: 遍历 slots + edges
  ErasedPluginRunner     新增  per-slot: behavior 循环 + hooks + backoff + memory

lattice-plugin
  Plugin trait           微扩  加 output_schema()
  ErasedPlugin           新增  类型擦除
  PluginRegistry         新增  插件注册表
  PluginBundle           新增  可分发形态
  PluginRunner           重构  提取 run_plugin_loop() 供 ErasedPluginRunner 复用
  builtin/               新增  9 个内置插件

lattice-agent
  Agent                  微改  加 set_system_prompt() 替换语义
  PluginAgent trait      微扩  加 send_message_with_tools()
  其余不变

lattice-core
  完全不变
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

    // [v0.5.0 新增] 声明式 JSON Schema。
    // AgentRunner 在没有 HandoffConfig.output_schema 时用它兜底校验。
    fn output_schema(&self) -> Option<serde_json::Value> { None }
}
```

### 3.2 ErasedPlugin

```rust
pub trait ErasedPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    fn to_prompt_json(&self, input: &serde_json::Value) -> Result<String, PluginError>;
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
    fn to_prompt_json(&self, input: &serde_json::Value) -> Result<String, PluginError> {
        let typed: T::Input = serde_json::from_value(input.clone())
            .map_err(|e| PluginError::Parse(format!(
                "{}: failed to deserialize input: {}", self.name(), e
            )))?;
        Ok(self.to_prompt(&typed))
    }

    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let typed = self.parse_output(raw)?;
        serde_json::to_value(typed).map_err(|e| PluginError::Parse(format!(
            "{}: failed to serialize output: {}", self.name(), e
        )))
    }

    // 其余方法直接委托
}
```

### 3.3 Behavior（不变）

```rust
pub trait Behavior: Send + Sync {
    fn decide(&self, confidence: f64) -> Action;       // Done | Retry
    fn on_error(&self, error: &PluginError, attempt: u32) -> ErrorAction; // Retry | Abort | Escalate
}

pub enum Action { Done, Retry }
pub enum ErrorAction { Retry, Abort, Escalate }
```

### 3.4 PluginBundle + PluginRegistry

```rust
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
}

pub struct PluginBundle {
    pub meta: PluginMeta,
    pub plugin: Box<dyn ErasedPlugin>,
    pub default_tools: Vec<ToolDefinition>,
    pub default_behavior: BehaviorMode,
}

pub enum BehaviorMode {
    Strict {
        confidence_threshold: f64,
        max_retries: u32,
        escalate_to: Option<String>,
    },
    Yolo,
}

impl BehaviorMode {
    pub fn to_behavior(&self) -> Box<dyn Behavior> {
        match self.clone() {
            BehaviorMode::Strict { confidence_threshold, max_retries, escalate_to } =>
                Box::new(StrictBehavior { confidence_threshold, max_retries, escalate_to }),
            BehaviorMode::Yolo => Box::new(YoloBehavior),
        }
    }
}

pub struct PluginRegistry {
    plugins: HashMap<String, PluginBundle>,
}

impl PluginRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, bundle: PluginBundle) -> Result<(), RegistryError>;
    pub fn get(&self, name: &str) -> Option<&PluginBundle>;
}
```

## 4. ErasedPluginRunner (lattice-plugin)

### 4.1 共享 run loop

```rust
// lattice-plugin/src/runner_core.rs

pub(crate) fn run_plugin_loop(
    plugin: &dyn ErasedPlugin,
    behavior: &dyn Behavior,
    agent: &mut dyn PluginAgent,
    initial_prompt: &str,
    config: &PluginConfig,
    hooks: Option<&dyn PluginHooks>,
    retry_policy: Option<&RetryPolicy>,
    memory: Option<&mut dyn Memory>,
) -> Result<RunResult, PluginError> {
    let mut attempt = 0u32;

    if let Some(h) = hooks {
        h.on_start(plugin.name(), (initial_prompt.len() as u32).div_ceil(4));
    }

    loop {
        if attempt >= config.max_turns {
            return Err(PluginError::MaxTurnsExceeded(config.max_turns));
        }

        let raw = agent
            .send_message_with_tools(initial_prompt)
            .map_err(|e| {
                let pe = PluginError::Other(e.to_string());
                // 即使是 LLM 调用错误，也让 behavior.on_error 判断
                pe
            })?;

        match plugin.parse_output_json(&raw) {
            Ok(output) => {
                let confidence = extract_confidence(&raw);  // ★ 改为 pub(crate)
                let action = behavior.decide(confidence);

                if let Some(h) = hooks {
                    h.on_turn(attempt, None, &action);
                }

                match action {
                    Action::Done => {
                        let json = serde_json::to_string(&output)
                            .map_err(|e| PluginError::Other(e.to_string()))?;
                        if json.len() > config.max_output_bytes {
                            return Err(PluginError::OutputTooLarge(
                                json.len(), config.max_output_bytes));
                        }

                        let result = RunResult {
                            output: json,
                            turns: attempt + 1,
                            final_action: Action::Done,
                        };
                        if let Some(h) = hooks { h.on_complete(&result); }
                        if let Some(mem) = memory {
                            save_memory_entries(mem, plugin.name(), initial_prompt, &result);
                        }
                        return Ok(result);
                    }
                    Action::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy {
                            std::thread::sleep(p.jittered_backoff(attempt));
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(h) = hooks { h.on_error(attempt, &e); }
                match behavior.on_error(&e, attempt) {
                    ErrorAction::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy {
                            std::thread::sleep(p.jittered_backoff(attempt));
                        }
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

// extract_confidence 从 fn 改为 pub(crate)，供 lattice-harness 也使用
pub(crate) fn extract_confidence(raw: &str) -> f64 { /* 现有实现不变 */ }
```

### 4.2 PluginAgent 新增方法

```rust
pub trait PluginAgent {
    /// 已有：单次 chat，不处理 tool call，每次 push user message
    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;

    /// [v0.5.0 新增] 发送 user message → 内部 tool loop → 收集最终文本。
    /// Agent 实现：调 self.run(message, max_turns) → 收集 Token 文本。
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;

    fn set_system_prompt(&mut self, prompt: &str);
    fn token_usage(&self) -> u64;
}

impl PluginAgent for Agent {
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>> {
        let events = self.run(message, 10);
        let mut text = String::new();
        for event in &events {
            if let LoopEvent::Token { text: t } = event {
                text.push_str(t);
            }
        }
        Ok(text)
    }
}
```

### 4.3 两版 Runner

```rust
// 泛型版（现有，重构为委托给 run_plugin_loop）
impl<'a, P: Plugin, B: Behavior, A: PluginAgent> PluginRunner<'a, P, B, A> {
    pub fn run(&mut self, input: &P::Input) -> Result<RunResult, PluginError> {
        let prompt = self.plugin.to_prompt(input);
        run_plugin_loop(
            &ErasedPluginAdapter(self.plugin),
            self.behavior,
            self.agent,
            &prompt,
            self.config,
            self.hooks,
            self.retry_policy,
            self.memory.as_deref_mut(),
        )
    }
}

// trait object 版（新增）
impl<'a> ErasedPluginRunner<'a> {
    pub fn run(&mut self, input: &serde_json::Value) -> Result<RunResult, PluginError> {
        let prompt = self.plugin.to_prompt_json(input)?;
        run_plugin_loop(
            self.plugin,
            self.behavior,
            self.agent,
            &prompt,
            self.config,
            self.hooks,
            self.retry_policy,
            self.memory.as_deref_mut(),
        )
    }
}
```

## 5. AgentProfile 扩展 (lattice-harness)

### 5.1 TOML

```toml
[agent]
name = "code-reviewer"
model = "sonnet"

[plugins]
entry = "review"

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

### 5.2 Rust 类型 + 转换

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorModeConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BehaviorModeConfig {
    pub mode: String,                       // "strict" | "yolo"
    #[serde(default)]
    pub confidence_threshold: Option<f64>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub escalate_to: Option<String>,
}

impl BehaviorModeConfig {
    /// TOML 配置 → BehaviorMode enum。
    /// mode="yolo" 时忽略其他字段；mode="strict" 时缺失字段用默认值。
    /// mode 不匹配任何已知值 → None。
    pub fn to_behavior_mode(&self) -> Option<BehaviorMode> {
        match self.mode.as_str() {
            "yolo" => Some(BehaviorMode::Yolo),
            "strict" => {
                Some(BehaviorMode::Strict {
                    confidence_threshold: self.confidence_threshold.unwrap_or(0.7),
                    max_retries: self.max_retries.unwrap_or(3),
                    escalate_to: self.escalate_to.clone(),
                })
            }
            _ => {
                tracing::warn!("Unknown behavior mode '{}', ignoring", self.mode);
                None
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginsConfig {
    pub entry: String,
    pub slots: Vec<PluginSlotConfig>,
    pub edges: Vec<AgentEdgeConfig>,
}
```

**entry 校验**：`AgentProfile::load()` 验证：
1. `entry` 指向的 slot 在 `slots` 中存在 → 否则返回 `LoadError::EntrySlotNotFound`
2. `edges` 中的 `from` 均指向已定义的 slot → 否则 warn
3. 存在可达的终止状态（某条 edge rule.default=true 且 target=None，或无匹配 edge 的 slot）→ warn

## 6. PluginDagRunner (lattice-harness)

### 6.1 常量

```rust
/// Plugin DAG 最大 slot 切换次数。防止死循环。
/// 不是 LLM 调用次数——是 slot → slot 的转移次数。
/// 与 PluginSlotConfig.max_turns（单个 slot 内 LLM 调用上限）正交。
const MAX_DAG_SLOT_TRANSITIONS: u32 = 50;
```

### 6.2 DAGError

```rust
#[derive(Debug, Error)]
pub enum DAGError {
    #[error("entry slot '{0}' not found in [plugins.slots]")]
    EntrySlotNotFound(String),

    #[error("slot '{0}' not found in [plugins.slots]")]
    SlotNotFound(String),

    #[error("plugin '{0}' not found in PluginRegistry")]
    PluginNotFound(String),

    #[error("model resolve failed: {0}")]
    Resolve(String),

    #[error("max slot transitions ({0}) exceeded — possible infinite loop")]
    MaxSlotTransitionsExceeded(u32),

    #[error("{0}")]
    Plugin(#[from] PluginError),

    #[error("{0}")]
    Parse(String),

    #[error("fork not supported in intra-agent DAG — use Pipeline fork:target")]
    ForkNotSupportedInDag,

    #[error("plugin '{plugin}' escalated after {after_attempts} attempts: {original}")]
    Escalated {
        plugin: String,
        after_attempts: u32,
        original: String,
    },
}
```

### 6.3 run() + find_edge()

```rust
impl PluginDagRunner<'_> {
    pub fn run(
        &mut self,
        initial_input: &str,
        default_model: &str,
    ) -> Result<serde_json::Value, DAGError> {
        // entry 校验在 AgentProfile::load() 已做
        let mut current_name = self.config.entry.clone();
        let mut current_input = serde_json::json!({"input": initial_input});
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

            // ── 每个 slot 重建 LlmAgent ──
            let model = slot.model_override.as_deref().unwrap_or(default_model);
            let resolved = resolve(model)
                .map_err(|e| DAGError::Resolve(e.to_string()))?;
            let mut llm = Agent::new(resolved);
            llm.set_system_prompt(bundle.plugin.system_prompt());

            let tool_defs = merge_tool_definitions(
                &self.tool_registry,
                &slot.tools,
                bundle.plugin.tools(),
            );
            llm = llm.with_tools(tool_defs);

            // ── ErasedPluginRunner ──
            let behavior = slot.behavior.as_ref()
                .and_then(|c| BehaviorModeConfig::to_behavior_mode(c.clone()))
                .unwrap_or_else(|| bundle.default_behavior.clone())
                .to_behavior();

            let plugin_config = PluginConfig {
                max_turns: slot.max_turns.unwrap_or(10),
                ..Default::default()
            };

            let mut runner = ErasedPluginRunner::new(
                bundle.plugin.as_ref(),
                behavior.as_ref(),
                &mut llm,
                &plugin_config,
                None,
                Some(&self.retry_policy),
                self.shared_memory.clone(),
            );

            let result = runner.run(&current_input)?;  // PluginError → DAGError via From
            let output_json: serde_json::Value = serde_json::from_str(&result.output)
                .map_err(|e| DAGError::Parse(e.to_string()))?;

            // ── 找下一个 slot ──
            let next = self.find_edge(&current_name, &output_json);

            match next {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    current_input = output_json;
                    transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => {
                    return Err(DAGError::ForkNotSupportedInDag);
                }
                None => return Ok(output_json),
            }
        }
    }

    /// 遍历 edges，找 from == current_name 且 rule.eval(output) == true 的第一条边。
    /// 返回 rule.target。无匹配返回 None（DAG 终点）。
    fn find_edge(
        &self,
        from: &str,
        output: &serde_json::Value,
    ) -> Option<HandoffTarget> {
        self.config.edges.iter()
            .filter(|e| e.from == from)
            .find(|e| e.rule.eval(output))
            .and_then(|e| e.rule.target.clone())
    }
}
```

### 6.4 input 传递语义

上游 JSON output 直接作为下游 JSON input（`serde_json::Value`）。下游 plugin 通过 `ErasedPlugin::to_prompt_json(input)` 转换。

```
review → {"issues": [...], "confidence": 0.9}
  ↓ (serde_json::Value 直接传递)
refactor → plugin.to_prompt_json({"issues": [...], "confidence": 0.9})
  → serde_json::from_value::<RefactorInput>
  → Refactor::to_prompt(&refactor_input)
```

## 7. ToolRegistry (lattice-harness)

```rust
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub handler: ToolHandler,
}

pub enum ToolHandler {
    Native(Arc<dyn Fn(serde_json::Value) -> Result<String, ToolError> + Send + Sync>),
    McpBacked { server: String, tool_name: String },
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("execution failed: {0}")]
    Execution(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("MCP server '{server}': {source}")]
    McpUnreachable { server: String, source: String },
    #[error("timeout after {0}ms")]
    Timeout(u64),
}

/// 合并三层工具定义。
/// - shared (ToolRegistry): 基础
/// - slot: Vec<String> — 在 ToolRegistry 中查找，不在则 warn + skip
/// - plugin: &[ToolDefinition] — 插件自带
/// 同名工具：plugin 覆盖 slot，slot 覆盖 shared
pub fn merge_tool_definitions(
    registry: &ToolRegistry,
    slot_tool_names: &[String],
    plugin_tools: &[ToolDefinition],
) -> Vec<ToolDefinition>;
```

`merge_tool_definitions` 实现：

```rust
pub fn merge_tool_definitions(
    registry: &ToolRegistry,
    slot_tool_names: &[String],
    plugin_tools: &[ToolDefinition],
) -> Vec<ToolDefinition> {
    use std::collections::IndexMap;
    let mut merged: IndexMap<String, ToolDefinition> = IndexMap::new();

    // slot 工具（从 ToolRegistry 解析名字）
    for name in slot_tool_names {
        match registry.get(name) {
            Some(tool) => { merged.insert(name.clone(), tool.definition.clone()); }
            None => {
                tracing::warn!("Tool '{}' referenced in slot but not found in ToolRegistry — skipping", name);
            }
        }
    }

    // plugin 工具（同名覆盖）
    for td in plugin_tools {
        merged.insert(td.function.name.clone(), td.clone());
    }

    merged.into_values().collect()
}
```

## 8. LlmAgent 微改 (lattice-agent)

### 8.1 set_system_prompt 替换语义

```rust
impl Agent {
    pub fn set_system_prompt(&mut self, prompt: &str) {
        let system_msg = Message {
            role: Role::System,
            content: prompt.to_string(),
            ..
        };
        match self.state.messages.first() {
            Some(msg) if msg.role == Role::System => {
                self.state.messages[0] = system_msg;
            }
            _ => {
                self.state.messages.insert(0, system_msg);
            }
        }
    }
}
```

### 8.2 PluginAgent::send_message_with_tools

见第 4.2 节。

## 9. 内置插件

| 插件 | Input | Output | 文件 |
|------|-------|--------|------|
| `CodeReview` | `CodeReviewInput` | `CodeReviewOutput { issues, confidence }` | builtin/code_review.rs |
| `Refactor` | `RefactorInput` | `RefactorOutput { refactored_code, changes }` | builtin/refactor.rs |
| `TestGen` | `TestGenInput` | `TestGenOutput { tests, coverage_estimate }` | builtin/test_gen.rs |
| `SecurityAudit` | `SecurityAuditInput` | `SecurityAuditOutput { vulnerabilities, risk_score }` | builtin/security_audit.rs |
| `DocGen` | `DocGenInput` | `DocGenOutput { documentation, sections }` | builtin/doc_gen.rs |
| `PptxGen` | `PptxGenInput` | `PptxGenOutput { slides, speaker_notes }` | builtin/pptx_gen.rs |
| `DeepResearch` | `DeepResearchInput` | `DeepResearchOutput { findings, citations, confidence }` | builtin/deep_research.rs |
| `ImageGen` | `ImageGenInput` | `ImageGenOutput { image_url, alt_text, metadata }` | builtin/image_gen.rs |
| `KnowledgeBase` | `KnowledgeBaseInput` | `KnowledgeBaseOutput { results, relevance_scores }` | builtin/knowledge_base.rs |

共享工具：`builtin/parse_utils.rs`。

## 10. 集成测试场景

最小端到端验证（第 13 轮）：

```rust
#[test]
fn test_plugin_dag_review_then_refactor() {
    // 1. 构造 PluginRegistry，注册 CodeReview + Refactor
    let mut registry = PluginRegistry::new();
    registry.register(PluginBundle {
        meta: PluginMeta { name: "CodeReview".into(), version: "0.1".into(), .. },
        plugin: Box::new(CodeReviewPlugin::new()),
        default_behavior: BehaviorMode::Yolo,
        default_tools: vec![],
    });
    registry.register(PluginBundle {
        meta: PluginMeta { name: "Refactor".into(), version: "0.1".into(), .. },
        plugin: Box::new(RefactorPlugin::new()),
        default_behavior: BehaviorMode::Yolo,
        default_tools: vec![],
    });

    // 2. 构造 DAG: review → (conf>0.5) → refactor → end
    let config = PluginsConfig {
        entry: "review".into(),
        slots: vec![
            PluginSlotConfig { name: "review".into(), plugin: "CodeReview".into(), .. },
            PluginSlotConfig { name: "refactor".into(), plugin: "Refactor".into(), .. },
        ],
        edges: vec![
            AgentEdgeConfig {
                from: "review".into(),
                rule: HandoffRule { condition: Some(HandoffCondition {
                    field: "confidence".into(), op: ">".into(), value: json!(0.5),
                }), target: Some(HandoffTarget::Single("refactor".into())), .. },
            },
            AgentEdgeConfig {
                from: "refactor".into(),
                rule: HandoffRule { default: true, .. },
            },
        ],
    };

    // 3. 执行
    let tool_registry = ToolRegistry::new();  // 空 registry
    let mut dag = PluginDagRunner::new(&config, &registry, &tool_registry);
    let result = dag.run("+unsafe code here", "mock-model");

    // 4. 验证：refactor 的 output 包含 refactored_code
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.get("refactored_code").is_some());
}

#[test]
fn test_entry_slot_not_found_is_error() {
    // AgentProfile::load() 拒绝 entry 不存在的配置
}

#[test]
fn test_missing_tool_is_warned_not_error() {
    // slot.tools 中有不存在的工具 → warn，继续执行
}
```

## 11. 实现顺序

```
第1轮:  ErasedPlugin trait + blanket impl
第2轮:  PluginBundle + PluginMeta + BehaviorMode + to_behavior()
第3轮:  PluginRegistry
第4轮:  Agent.set_system_prompt() 替换语义
第5轮:  PluginAgent::send_message_with_tools()
第6轮:  重构 PluginRunner: 提取 run_plugin_loop() → ErasedPluginRunner
第7轮:  PluginsConfig + PluginSlotConfig + AgentEdgeConfig + BehaviorModeConfig
第8轮:  PluginDagRunner + DAGError + find_edge
第9轮:  AgentRunner 集成
第10轮: ToolRegistry + merge_tool_definitions
第11轮: parse_utils + 3 个内置插件（Refactor, TestGen, SecurityAudit）
第12轮: 剩余 6 个内置插件
第13轮: 集成测试 + 文档
```

## 12. 与现有代码的关系

### 不动

- `lattice-core`: 全部
- `lattice-agent`: AgentState, Memory trait, 7 个内置工具
- `lattice-harness`: Pipeline, HandoffRule, HandoffTarget, HandoffCondition, eval_rules, AgentRegistry, EventBus, Watcher, WebSocket, dry_run
- `lattice-plugin`: Behavior trait, StrictBehavior, YoloBehavior, PluginHooks, PluginConfig, RunResult, PluginError, Action, ErrorAction

### 修改

- `lattice-plugin/src/lib.rs`: Plugin trait 加 `output_schema()`；`extract_confidence` 改为 `pub(crate)`
- `lattice-plugin/src/runner.rs`: 重构，提取 `run_plugin_loop()` 共享函数
- `lattice-agent/src/agent.rs`: 加 `set_system_prompt()`
- `lattice-agent/src/lib.rs`: PluginAgent trait 加 `send_message_with_tools()` + Agent impl
- `lattice-harness/src/profile.rs`: 加 PluginsConfig 等 + BehaviorModeConfig::to_behavior_mode()
- `lattice-harness/src/runner.rs`: AgentRunner 加 plugin_registry/tool_registry 字段 + run() 分支
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`

### 新增

- `lattice-plugin/src/erased.rs`: ErasedPlugin trait + blanket impl
- `lattice-plugin/src/registry.rs`: PluginRegistry
- `lattice-plugin/src/bundle.rs`: PluginBundle, PluginMeta, BehaviorMode
- `lattice-plugin/src/erased_runner.rs`: ErasedPluginRunner
- `lattice-plugin/src/builtin/`: 9 个插件 + parse_utils.rs + mod.rs
- `lattice-harness/src/dag_runner.rs`: PluginDagRunner + DAGError
- `lattice-harness/src/tools.rs`: ToolRegistry + ToolError + merge_tool_definitions

## 13. 不在此 spec

- Swarm 去中心化编排
- Python 胶水层 + pip 分发
- 运行时动态加载 (.wasm / .so)
- intra-agent Fork（用 Pipeline Fork 替代）
- Agent 工具执行层重构（ToolRegistry 集成延后）
