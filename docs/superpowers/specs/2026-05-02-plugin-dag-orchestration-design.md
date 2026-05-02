# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.3.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness`（+ `lattice-agent` 微改）

## 1. 核心设计决定

### 1.1 两套编排模型的关系

**Plugin DAG 是 intra-agent 执行引擎，Pipeline 是 inter-agent 编排器。不并列，分层。**

```
Pipeline (不变)
  │
  ├─ Agent A ("code-review")
  │     └─ 内部: Plugin DAG (slots + edges)   ← 新增能力
  │          review_slot → refactor_slot → summarize_slot
  │
  ├─ handoff eval_rules → Agent B ("deploy-check")
  │     └─ 内部: 简单模式 (现有 system prompt)  ← 不变
  │
  └─ handoff eval_rules → None (结束)
```

- **Pipeline**: 不变。仍是唯一 inter-agent 编排入口。调度 AgentRegistry 中的 TOML agent，eval_rules 决定下一个 agent。
- **Plugin DAG**: AgentProfile 的可选替代内部实现。当 TOML 中 `[plugins]` 段存在时，AgentRunner 走 Plugin DAG；否则走原始 system prompt 路径。
- **AgentRegistry**: 不变。仍是 agent 的注册表（TOML 文件）。
- **PluginRegistry**: 新增。插件能力注册表（Rust 代码）。AgentProfile 通过 `plugin = "CodeReview"` 引用。

### 1.2 为什么不用 Plugin DAG 替代 Pipeline

- Pipeline 已有完整 inter-agent 编排：handoff 规则评估、Fork 并行、错误处理、dry-run 验证、WebSocket 事件、热重载
- Plugin DAG 解决的是另一个问题：agent 内部如何用类型化插件组织推理逻辑
- 两层各司其职，不替代

## 2. 架构分层

```
lattice-harness
  Pipeline (不变)           ← inter-agent: AgentProfile → AgentRunner → eval_rules → next agent
  AgentRunner (扩展)        ← 检测 [plugins] → 走 PluginDagRunner，否则走原始 Agent
  PluginDagRunner (新增)    ← intra-agent: 按 slots+edges 执行 Plugin DAG
  AgentProfile (扩展)       ← 加 [plugins] 段，可选

lattice-plugin
  Plugin trait (微扩)       ← 加 output_schema()
  ErasedPlugin (新增)       ← 类型擦除，进 PluginRegistry
  PluginRegistry (新增)     ← 插件注册表
  PluginBundle (新增)       ← 插件可分发形态
  builtin/ (新增)           ← 8 个内置插件

lattice-agent
  LlmAgent (微改)           ← 加 set_system_prompt() 替换语义，加 with_tools() 已有
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

    // [0.3.0 新增]
    // 声明式 JSON Schema。AgentRunner 已有 schema validation（HandoffConfig.output_schema），
    // 那层是执行性的。Plugin::output_schema() 是声明性的——描述这个插件产出什么形状的 JSON，
    // 供下游 slot to_prompt 时参考，也供 AgentRunner 在没有 HandoffConfig.output_schema 时兜底。
    fn output_schema(&self) -> Option<serde_json::Value> { None }
}
```

**与 HandoffConfig.output_schema 的关系**：
- `Plugin::output_schema()` — 声明性，"我产出这个形状"
- `HandoffConfig.output_schema` — 执行性，"AgentRunner 用 jsonschema 校验 + 重试"
- 如果 TOML 有 output_schema → AgentRunner 用它校验（不变）
- 如果 TOML 无 output_schema 但 Plugin 有 → AgentRunner 用 Plugin 的兜底校验
- 如果都没有 → 不校验（现有行为）

### 3.2 ErasedPlugin（类型擦除）

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
                "Failed to deserialize input for {}: {}", self.name(), e
            )))?;
        Ok(self.to_prompt(&typed))
    }

    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let typed = self.parse_output(raw)?;
        serde_json::to_value(typed)
            .map_err(|e| PluginError::Parse(format!(
                "Failed to serialize output for {}: {}", self.name(), e
            )))
    }

    // name, system_prompt, tools, preferred_model, output_schema 直接委托
}
```

**JSON 转换精度问题**：f64 ↔ serde_json::Number 可能因为 NaN/Infinity 失败。`parse_output()` 返回的 Output 应避免 NaN/Infinity。文档标注此约束。

### 3.3 Behavior

**不变**。现有 `Behavior` trait 和 `StrictBehavior`/`YoloBehavior` 不动。

```rust
pub trait Behavior: Send + Sync {
    fn decide(&self, confidence: f64) -> Action;
    fn on_error(&self, error: &PluginError, attempt: u32) -> ErrorAction;
}

pub enum Action { Done, Retry }
pub enum ErrorAction { Retry, Abort, Escalate }
```

### 3.4 PluginRegistry + PluginBundle

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
        escalate_to: Option<String>,  // 重试耗尽后 Escalate（不是 Abort）
    },
    Yolo,
}

impl BehaviorMode {
    /// 转换为 trait object，供 PluginRunner 使用
    pub fn to_behavior(&self) -> Box<dyn Behavior> {
        match self.clone() {
            BehaviorMode::Strict { confidence_threshold, max_retries, escalate_to } => {
                Box::new(StrictBehavior { confidence_threshold, max_retries, escalate_to })
            }
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
    pub fn list(&self) -> Vec<&PluginMeta>;
}
```

## 4. AgentProfile 扩展 (lattice-harness)

### 4.1 TOML 配置

```toml
# agent.toml — 插件模式
[agent]
name = "code-reviewer"
model = "sonnet"

[plugins]
entry = "review"

[[plugins.slots]]
name = "review"
plugin = "CodeReview"
max_turns = 3

[[plugins.slots]]
name = "refactor"
plugin = "Refactor"
model_override = "sonnet"      # 可选：这个 slot 用不同模型
max_turns = 5

[[plugins.edges]]
from = "review"
rule = { condition = { field = "confidence", op = ">", value = "0.7" }, target = "refactor" }

[[plugins.edges]]
from = "review"
rule = { default = true }      # confidence <= 0.7 → 结束（无 target）

[[plugins.edges]]
from = "refactor"
rule = { default = true }      # refactor 完成 → 结束

[handoff]                      # inter-agent handoff（不变）
fallback = "deploy-check"
```

无 `[plugins]` 段的 agent.toml 走现有原始 system prompt 路径，完全向后兼容。

### 4.2 Rust 类型

```rust
// AgentProfile 扩展
pub struct AgentProfile {
    pub agent: AgentConfig,
    pub system: SystemConfig,              // 插件模式下可选（兜底 system prompt）
    pub tools: ToolsConfig,
    pub behavior: BehaviorConfig,
    pub handoff: HandoffConfig,
    pub plugins: Option<PluginsConfig>,     // [0.3.0 新增]
    pub bus: BusConfigProfile,
    pub memory: MemoryConfigProfile,
}

pub struct PluginsConfig {
    pub entry: String,
    pub slots: Vec<PluginSlotConfig>,
    pub edges: Vec<AgentEdgeConfig>,
}

pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,                    // PluginRegistry key
    pub tools: Vec<String>,                // 工具名列表（ToolRegistry 解析）
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,            // 默认 10
    pub behavior: Option<BehaviorModeConfig>,
}

pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,                 // 复用现有 HandoffRule，含 target
}

pub struct BehaviorModeConfig {
    pub mode: String,                      // "strict" | "yolo"
    pub confidence_threshold: Option<f64>,
    pub max_retries: Option<u32>,
    pub escalate_to: Option<String>,
}
```

**entry 校验**：AgentProfile::load() 时检查 entry 指向的 slot 存在，否则返回加载错误（非运行时 panic）。

## 5. PluginDagRunner (lattice-harness, 新增)

### 5.1 职责

AgentRunner 检测 `profile.plugins`：
- `Some(plugins_config)` → 构造 `PluginDagRunner`，执行 Plugin DAG
- `None` → 走现有原始 `llm_agent.run()` 路径

```rust
// AgentRunner::run() 扩展
pub fn run(&mut self, input: &str, max_turns: u32) -> Result<serde_json::Value, Box<dyn Error>> {
    if let Some(ref plugins_config) = self.profile.plugins {
        let mut dag = PluginDagRunner::new(
            plugins_config,
            &self.plugin_registry,   // AgentRunner 新增字段
            &self.tool_registry,     // AgentRunner 新增字段
            self.shared_memory.clone(),
        );
        let output = dag.run(input, &self.profile.agent.model)?;
        // schema validation 不变（HandoffConfig.output_schema 或 Plugin::output_schema 兜底）
        self.validate_output(output, max_turns)
    } else {
        // 现有路径，不变
        self.run_raw(input, max_turns)
    }
}
```

### 5.2 PluginDagRunner 执行流程

```rust
pub struct PluginDagRunner<'a> {
    config: &'a PluginsConfig,
    plugin_registry: &'a PluginRegistry,
    tool_registry: &'a ToolRegistry,
    shared_memory: Option<Arc<dyn Memory>>,
}

impl PluginDagRunner<'_> {
    pub fn run(&mut self, initial_input: &str, default_model: &str) -> Result<serde_json::Value> {
        let mut current_name = &self.config.entry;
        // 初始 input 包装为 JSON: {"input": "..."}
        let mut current_input = serde_json::json!({"input": initial_input});
        let mut total_slot_transitions = 0u32;

        loop {
            if total_slot_transitions >= MAX_DAG_TURNS { return Err(...); }

            let slot = self.config.slots.iter().find(|s| s.name == *current_name)
                .ok_or_else(|| DAGError::SlotNotFound(current_name.clone()))?;

            let bundle = self.plugin_registry.get(&slot.plugin)
                .ok_or_else(|| DAGError::PluginNotFound(slot.plugin.clone()))?;

            // ── 每个 slot 重建 LlmAgent ──
            let model = slot.model_override.as_deref().unwrap_or(default_model);
            let resolved = resolve(model)?;
            let mut llm = LlmAgent::new(resolved);

            // 替换（非追加）system prompt
            llm.set_system_prompt(bundle.plugin.system_prompt());

            // 合并工具 + builder 模式注入
            let tools = merge_tools(
                &self.tool_registry,
                &slot.tools,
                bundle.plugin.tools(),
            );
            llm = llm.with_tools(tools);

            // ── PluginRunner 内循环 ──
            let behavior = slot.behavior.clone()
                .unwrap_or(bundle.default_behavior.clone())
                .to_behavior();
            let max_turns = slot.max_turns.unwrap_or(10);

            let output_json = self.run_plugin_loop(
                bundle.plugin.as_ref(),
                behavior.as_ref(),
                &mut llm,
                &current_input,
                max_turns,
            )?;

            // ── 找下一个 slot ──
            let next = self.find_edge(current_name, &output_json)?;

            match next {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    current_input = output_json;  // 上游 JSON output = 下游 JSON input
                    total_slot_transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => {
                    // Fork 不在 intra-agent DAG 层实现。
                    // Pipeline 已有完整的 Fork 支持（std::thread::spawn + merge）。
                    // 如需 intra-agent 并行，用多个 agent + Pipeline Fork。
                    return Err(DAGError::ForkNotSupportedInDag);
                }
                None => return Ok(output_json),  // DAG 终点
            }
        }
    }

    /// PluginRunner 内循环：Behavior decide/on_error 驱动
    fn run_plugin_loop(
        &self,
        plugin: &dyn ErasedPlugin,
        behavior: &dyn Behavior,
        llm: &mut LlmAgent,
        input: &serde_json::Value,
        max_turns: u32,
    ) -> Result<serde_json::Value> {
        let prompt = plugin.to_prompt_json(input)?;
        let mut attempt = 0u32;

        loop {
            if attempt >= max_turns { return Err(DAGError::MaxSlotTurnsExceeded); }

            // Agent::send() 内部已处理 tool loop — Plugin 不需要手动管理工具
            let raw = llm.send(&prompt)
                .map_err(|e| PluginError::Other(e.to_string()))?;

            match plugin.parse_output_json(&raw) {
                Ok(output) => {
                    let confidence = extract_confidence(&raw);
                    match behavior.decide(confidence) {
                        Action::Done => return Ok(output),
                        Action::Retry => {
                            attempt += 1;
                            continue;
                        }
                    }
                }
                Err(e) => {
                    match behavior.on_error(&e, attempt) {
                        ErrorAction::Retry => { attempt += 1; continue; }
                        ErrorAction::Abort => return Err(e.into()),
                        ErrorAction::Escalate => {
                            return Err(DAGError::Escalated {
                                plugin: plugin.name().into(),
                                after_attempts: attempt,
                                original: e.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
}
```

### 5.3 input 传递语义

**上游 JSON output 直接作为下游 JSON input**。不转字符串、不嵌入 prompt。

```
review slot 产出: {"issues": [...], "confidence": 0.9}
       ↓ (serde_json::Value 直接传递)
refactor slot: plugin.to_prompt_json({"issues": [...], "confidence": 0.9})
       → 插件内部自己决定如何把 Input 字段映射到 prompt
```

每个 Plugin 的 `to_prompt()` 接收完整的、类型化的 Input struct，由插件的 `to_prompt` 实现决定如何拼 prompt。上游产出物被 `serde_json::from_value` 转成下游的 `Input` 类型。如果转换失败 → `PluginError::Parse`。

### 5.4 Fork 策略

**intra-agent DAG 不支持 Fork。Fork 是 inter-agent 概念，在 Pipeline 层做。**

如需并行：把多个 slot 拆成独立 agent，用 Pipeline Fork。
```
# 不用 DAG 内 Fork：
#   review → fork:security,perf → merge → summarize

# 而是用 Pipeline：
[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.7" }
target = "fork:security-agent,perf-agent"
```

Pipeline 的 fork 实现不变。

## 6. LlmAgent 微改 (lattice-agent)

### 6.1 set_system_prompt 替换语义

```rust
impl LlmAgent {
    /// 替换已有的 system message（而非追加）。
    /// 如果 messages 第一条是 system，替换它；否则插入到最前面。
    pub fn set_system_prompt(&mut self, prompt: &str) {
        let system_msg = Message {
            role: Role::System,
            content: prompt.to_string(),
            ..
        };
        match self.state.messages.first_mut() {
            Some(msg) if msg.role == Role::System => *msg = system_msg,
            _ => self.state.messages.insert(0, system_msg),
        }
    }
}
```

`with_tools()` 已存在（builder 模式），不需要 `set_tools()`。

## 7. ToolRegistry (lattice-harness, 新增)

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
    #[error("Tool execution failed: {0}")]
    Execution(String),
    #[error("Tool not found: {0}")]
    NotFound(String),
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("MCP server '{server}' unreachable: {source}")]
    McpUnreachable { server: String, source: String },
    #[error("Timeout after {0}ms")]
    Timeout(u64),
}

impl ToolRegistry {
    pub fn register(&mut self, name: &str, handler: ToolHandler, definition: ToolDefinition);
    pub fn register_mcp(&mut self, server: &str, tool_name: &str);
    pub fn get(&self, name: &str) -> Option<&RegisteredTool>;
    pub fn dispatch(&self, tool_name: &str, args: serde_json::Value) -> Result<String, ToolError>;
}

/// 合并三层工具定义，Plugin 覆盖 slot，slot 覆盖 shared
pub fn merge_tool_definitions(
    shared: &[ToolDefinition],
    slot: &[String],              // ToolRegistry key
    plugin: &[ToolDefinition],
    registry: &ToolRegistry,
) -> Vec<ToolDefinition>;
```

**与 Agent::send() 内部 tool loop 的关系**：`LlmAgent.send()` 内部已通过 `with_tools()` 持有工具列表并自动执行 tool loop。ToolRegistry 负责工具的执行体——Agent 在收到 tool_call 时通过 ToolRegistry.dispatch() 执行具体工具。目前 Agent 内部工具执行在 `lattice-agent` 内置。ToolRegistry 是 harness 层的工具执行抽象，后续可注入给 Agent。

> 注：Agent 内部 tool 执行重构不在本 spec。本 spec 先定义 ToolRegistry 的数据结构和接口，Agent 集成放到第 6 轮。

## 8. 内置插件

| 插件 | Input struct | Output struct | 文件 |
|------|-------------|---------------|------|
| `CodeReview` | `CodeReviewInput` | `CodeReviewOutput { issues, confidence }` | 已有，迁移到 builtin/ |
| `Refactor` | `RefactorInput` | `RefactorOutput { refactored_code, changes }` | builtin/refactor.rs |
| `TestGen` | `TestGenInput` | `TestGenOutput { tests, coverage_estimate }` | builtin/test_gen.rs |
| `SecurityAudit` | `SecurityAuditInput` | `SecurityAuditOutput { vulnerabilities, risk_score }` | builtin/security_audit.rs |
| `DocGen` | `DocGenInput` | `DocGenOutput { documentation, sections }` | builtin/doc_gen.rs |
| `PptxGen` | `PptxGenInput` | `PptxGenOutput { slides, speaker_notes }` | builtin/pptx_gen.rs |
| `DeepResearch` | `DeepResearchInput` | `DeepResearchOutput { findings, citations, confidence }` | builtin/deep_research.rs |
| `ImageGen` | `ImageGenInput` | `ImageGenOutput { image_url, alt_text, metadata }` | builtin/image_gen.rs |
| `KnowledgeBase` | `KnowledgeBaseInput` | `KnowledgeBaseOutput { results, relevance_scores }` | builtin/knowledge_base.rs |

每个插件独立文件：`builtin/<name>.rs`。共享的 parse 工具函数（markdown 清洗、JSON 提取）放在 `builtin/parse_utils.rs`。

## 9. 实现顺序

```
第1轮:  ErasedPlugin trait + blanket impl (lattice-plugin)
第2轮:  PluginBundle + PluginMeta + BehaviorMode + to_behavior() (lattice-plugin)
第3轮:  PluginRegistry (lattice-plugin)
第4轮:  Agent.set_system_prompt() 替换语义 (lattice-agent)
第5轮:  PluginSlotConfig + AgentEdgeConfig + PluginsConfig (lattice-harness profile 扩展)
第6轮:  ToolRegistry + ToolError + merge_tool_definitions (lattice-harness)
第7轮:  PluginDagRunner::run() + run_plugin_loop() (lattice-harness)
第8轮:  AgentRunner 集成：检测 [plugins] → PluginDagRunner，否则走原路径 (lattice-harness)
第9轮:  AgentProfile::load() 扩展：解析 [plugins] 段 + entry 校验
第10轮: 3 个内置插件 (Refactor, TestGen, SecurityAudit) + parse_utils
第11轮: 剩余 5 个内置插件
第12轮: 测试 + 集成 + 文档
```

## 10. 与现有代码的关系

### 不动

- `lattice-core`: 全部
- `lattice-agent`: AgentState, PluginAgent trait, Memory trait, 工具执行（7 个内置工具）, Agent::send() tool loop
- `lattice-harness`: Pipeline, HandoffRule, HandoffTarget, HandoffCondition, eval_rules, AgentRegistry, EventBus, Watcher, WebSocket, dry_run
- `lattice-plugin`: Plugin trait（仅加 output_schema 默认方法）, Behavior trait, StrictBehavior, YoloBehavior, PluginRunner, PluginHooks, PluginConfig, PluginError, CodeReviewPlugin（迁移到 builtin/）

### 新增

- `lattice-plugin/src/erased.rs`: ErasedPlugin + blanket impl
- `lattice-plugin/src/registry.rs`: PluginRegistry
- `lattice-plugin/src/bundle.rs`: PluginBundle, PluginMeta, BehaviorMode
- `lattice-plugin/src/builtin/mod.rs` + 9 个插件文件 + `parse_utils.rs`
- `lattice-harness/src/dag_runner.rs`: PluginDagRunner
- `lattice-harness/src/tools.rs`: ToolRegistry, ToolError, merge_tool_definitions

### 修改

- `lattice-plugin/src/lib.rs`: `pub mod erased; pub mod registry; pub mod bundle; pub mod builtin;`
- `lattice-plugin/Cargo.toml`: 无新依赖
- `lattice-harness/src/profile.rs`: AgentProfile 加 `plugins: Option<PluginsConfig>`, PluginsConfig, PluginSlotConfig, AgentEdgeConfig, BehaviorModeConfig
- `lattice-harness/src/runner.rs`: AgentRunner 加 `plugin_registry: Option<Arc<PluginRegistry>>`, `tool_registry: Option<Arc<ToolRegistry>>`；run() 检测 plugins 分支
- `lattice-harness/src/lib.rs`: 加 `pub mod dag_runner; pub mod tools;`
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`
- `lattice-agent/src/agent.rs`: 加 `set_system_prompt()` 方法

## 11. 不在此 spec

- Swarm 去中心化编排
- Python 胶水层 + pip 分发
- 运行时动态加载 (.wasm / .so)
- Pipeline 替代或重写（Pipeline 不变）
- intra-agent Fork（用 Pipeline Fork 替代）
- Agent 工具执行层重构（ToolRegistry 集成到 Agent 内部）
