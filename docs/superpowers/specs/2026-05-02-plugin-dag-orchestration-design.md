# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.4.0
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
- **PluginDagRunner**：新增的 intra-agent 执行器。当 AgentProfile 有 `[plugins]` 段时，AgentRunner 委托给它。
- **ErasedPluginRunner**：PluginRunner 的类型擦除版。PluginDagRunner 每个 slot 用 ErasedPluginRunner 执行（而非重写循环）。

### 1.2 PluginRunner 和 ErasedPluginRunner

现有 `PluginRunner<P: Plugin, B: Behavior, A: PluginAgent>` 已有完整的 behavior 循环 + hooks + retry_policy + memory + output 校验。不复刻。

新增 `ErasedPluginRunner` —— 相同的循环逻辑，但参数类型擦除：

```rust
// 现有：类型参数
PluginRunner<'a, P: Plugin, B: Behavior, A: PluginAgent>

// 新增：trait object（进 PluginRegistry 用）
ErasedPluginRunner<'a> {
    plugin: &'a dyn ErasedPlugin,
    behavior: &'a dyn Behavior,
    agent: &'a mut dyn PluginAgent,
    config: &'a PluginConfig,
    hooks: Option<&'a dyn PluginHooks>,
    retry_policy: Option<&'a RetryPolicy>,
    memory: Option<Box<dyn Memory>>,
}
```

两者共享同一个 run loop 实现（宏或泛型辅助函数提取）。

### 1.3 PluginAgent::send() 的局限

`PluginAgent::send()` 只做一次 LLM 调用，不处理 tool call，且每次调用 `push_user_message`。重试用 `send()` 会累积 user message。

**PluginRunner/ErasedPluginRunner 不改用 send()**。改用 `Agent::run()`（含 tool loop）：

- `Agent::run(input, max_turns)` 内部：user message → chat → tool_call → submit_tools → chat → ... → final text
- 重试策略：build prompt once → agent.run() → parse → behavior.decide。如果 Retry，用同一个 Agent 再 run()（messages 自然累积，LLM 看到"上次不够好，再试一次"的上下文）
- Agent 重建时机：每个 slot 开始时重建（system prompt 干净，tools 精确）

> **注**：当前 PluginRunner 使用 `agent.send()`。Spec 要求改为使用 `agent.run()` 或等价的 tool-loop 方法。如果 PluginAgent trait 未来加 `run_with_tools()` 则直接迁移；否则 PluginRunner 直接依赖 `Agent` 具体类型而非 `PluginAgent` trait。

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
  PluginRunner           重构  提取 run loop 逻辑供 ErasedPluginRunner 复用
  builtin/               新增  9 个内置插件

lattice-agent
  Agent                  微改  加 set_system_prompt() 替换语义
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

    // [新增] 声明式 JSON Schema，描述 Output 的形状。
    // AgentRunner 在没有 HandoffConfig.output_schema 时用它兜底校验。
    fn output_schema(&self) -> Option<serde_json::Value> { None }
}
```

**output_schema 两层关系**：
- `Plugin::output_schema()` — 声明性，"我这个插件产出的 JSON 长这样"
- `HandoffConfig.output_schema` — 执行性，"AgentRunner 用 jsonschema 校验 + 最多 2 次修正重试"
- 如果 TOML 有 output_schema → 用它校验
- 如果 TOML 无但 Plugin 有 → 用 Plugin 的兜底
- 都没有 → 不校验

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

**JSON 转换注意**：`f64::NaN` / `Infinity` 会导致 `serde_json::to_value` 失败。Plugin 的 Output 不应包含这些值。

### 3.3 Behavior（不变）

```rust
pub trait Behavior: Send + Sync {
    fn decide(&self, confidence: f64) -> Action;
    fn on_error(&self, error: &PluginError, attempt: u32) -> ErrorAction;
}
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
    pub fn list(&self) -> Vec<&PluginMeta>;
}
```

## 4. ErasedPluginRunner (lattice-plugin)

### 4.1 设计原则

不复刻循环。提取现有 PluginRunner 的 run loop 为共享实现，`PluginRunner<P, B, A>` 和 `ErasedPluginRunner` 都调用它。

### 4.2 共享 run loop

```rust
// lattice-plugin/src/runner_core.rs

/// 共享的 PluginRunner run loop。
/// - plugin: 提供 prompt + 解析输出
/// - agent:  调用 LLM（含 tool loop）
/// - behavior: 决定 Done/Retry/Escalate
/// - hooks/retry_policy/memory/config: 可选增强
///
/// agent.send_message_then_collect() 发一次 user message + run_chat + tool loop → 收集文本。
/// 重试时用同一个 agent（messages 累积，LLM 自然看到修正上下文）。
/// 重试间应用 jittered backoff。
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
            .send_message_with_tools(&initial_prompt)  // ★ 需新增：含 tool loop 的 send
            .map_err(|e| {
                let pe = PluginError::Other(e.to_string());
                // 让 behavior 判断是否可重试
                pe
            })?;

        match plugin.parse_output_json(&raw) {
            Ok(output) => {
                let confidence = extract_confidence(&raw);
                let action = behavior.decide(confidence);

                if let Some(h) = hooks {
                    h.on_turn(attempt, None, &action);
                }

                match action {
                    Action::Done => {
                        // output size 检查
                        let json = serde_json::to_string(&output)
                            .map_err(|e| PluginError::Other(e.to_string()))?;
                        if json.len() > config.max_output_bytes {
                            return Err(PluginError::OutputTooLarge(
                                json.len(), config.max_output_bytes));
                        }

                        let result = RunResult { output: json, turns: attempt + 1, final_action: Action::Done };
                        if let Some(h) = hooks { h.on_complete(&result); }
                        if let Some(mem) = memory { save_to_memory(mem, plugin.name(), &initial_prompt, &result); }
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
```

### 4.3 send_message_with_tools — PluginAgent 新增方法

```rust
pub trait PluginAgent {
    /// 发送 user message → 内部 tool loop → 收集最终文本。
    /// 与 send() 的区别：send() 只做一次 chat + 忽略 ToolCallRequired；
    /// 本方法内部循环直到 LLM 不再请求 tool call 或达到 max_turns。
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;

    /// 已有：单次 chat，不处理 tool call
    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;

    // 已有
    fn set_system_prompt(&mut self, prompt: &str);
    fn token_usage(&self) -> u64;
}

// Agent 实现
impl PluginAgent for Agent {
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>> {
        let events = self.run(message, 10);  // Agent::run 有完整 tool loop
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

### 4.4 类型参数版和类型擦除版

```rust
// 类型参数版（现有，重构为委托给 run_plugin_loop）
impl<'a, P: Plugin, B: Behavior, A: PluginAgent> PluginRunner<'a, P, B, A> {
    pub fn run(&mut self, input: &P::Input) -> Result<RunResult, PluginError> {
        let prompt = self.plugin.to_prompt(input);
        run_plugin_loop(
            &ErasedPluginAdapter(self.plugin),  // P → ErasedPlugin 适配
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

// 类型擦除版（新增，PluginDagRunner 用）
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
rule = { default = true }       # 无条件结束（target 省略）

[[plugins.edges]]
from = "refactor"
rule = { default = true }

[handoff]                        # inter-agent（不变）
fallback = "deploy-check"
```

### 5.2 Rust 类型

```rust
// AgentProfile 扩展
pub struct AgentProfile {
    pub agent: AgentConfig,
    pub system: SystemConfig,
    pub tools: ToolsConfig,
    pub behavior: BehaviorConfig,
    pub handoff: HandoffConfig,
    pub plugins: Option<PluginsConfig>,   // [新增]
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
    pub plugin: String,                   // PluginRegistry key
    pub tools: Vec<String>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorModeConfig>,
}

pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: HandoffRule,                // 复用：condition + all + any + default + target
}

pub struct BehaviorModeConfig {
    pub mode: String,                     // "strict" | "yolo"
    pub confidence_threshold: Option<f64>,
    pub max_retries: Option<u32>,
    pub escalate_to: Option<String>,
}
```

**entry 校验**：`AgentProfile::load()` 时检查 entry 指向已定义的 slot，否则返回加载错误。

## 6. PluginDagRunner (lattice-harness)

### 6.1 职责

顺序遍历 Plugin DAG。每个 slot 委托给 `ErasedPluginRunner`。

```rust
const MAX_DAG_SLOT_TRANSITIONS: u32 = 50;

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
        let mut current_name = self.config.entry.clone();
        // 初始 input 包装为 JSON
        let mut current_input = serde_json::json!({"input": initial_input});
        let mut transitions = 0u32;

        loop {
            if transitions >= MAX_DAG_SLOT_TRANSITIONS {
                return Err(DAGError::MaxSlotTransitionsExceeded);
            }

            let slot = self.find_slot(&current_name)?;
            let bundle = self.plugin_registry.get(&slot.plugin)
                .ok_or_else(|| DAGError::PluginNotFound(slot.plugin.clone()))?;

            // ── 每个 slot 重建 LlmAgent ──
            let model = slot.model_override.as_deref().unwrap_or(default_model);
            let resolved = resolve(model).map_err(|e| DAGError::Resolve(e.to_string()))?;
            let mut llm = Agent::new(resolved);
            llm.set_system_prompt(bundle.plugin.system_prompt());

            let tool_defs = merge_tool_definitions(
                &self.tool_registry,
                &slot.tools,
                bundle.plugin.tools(),
            );
            llm = llm.with_tools(tool_defs);

            // ── ErasedPluginRunner（复用 PluginRunner 的完整循环）──
            let behavior = slot.behavior.clone()
                .and_then(|c| BehaviorModeConfig::to_behavior_mode(c))
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
                None,                            // hooks: 后续加
                Some(&self.retry_policy),
                self.shared_memory.clone(),
            );

            let result = runner.run(&current_input)?;
            let output_json: serde_json::Value = serde_json::from_str(&result.output)
                .map_err(|e| DAGError::Parse(e.to_string()))?;

            // ── 找下一个 slot ──
            let next_edge = self.config.edges.iter()
                .find(|e| e.from == current_name && e.rule.eval(&output_json));

            match next_edge.and_then(|e| e.rule.target.clone()) {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    current_input = output_json;  // 上游 JSON → 下游 JSON
                    transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => {
                    // intra-agent DAG 不支持 Fork。
                    // Fork 是 inter-agent 概念，在 Pipeline 层做。
                    return Err(DAGError::ForkNotSupportedInDag);
                }
                None => return Ok(output_json),
            }
        }
    }
}
```

### 6.2 input 传递语义

上游 JSON output 直接作为下游 JSON input（`serde_json::Value`）。下游 plugin 通过 `ErasedPlugin::to_prompt_json(input)` 接收，内部 `serde_json::from_value` 转为类型化 Input。

```
review → {"issues": [...], "confidence": 0.9}
  ↓
refactor → plugin.to_prompt_json({"issues": [...], "confidence": 0.9})
  → serde_json::from_value::<RefactorInput>
  → Refactor::to_prompt(&refactor_input)
```

### 6.3 Fork 策略

intra-agent DAG 不做 Fork。并行编排用 Pipeline Fork：

```toml
[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.7" }
target = "fork:security-agent,perf-agent"
```

## 7. AgentRunner 集成 (lattice-harness)

```rust
impl AgentRunner {
    pub fn run(
        &mut self,
        input: &str,
        max_turns: u32,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        if let Some(ref plugins_config) = self.profile.plugins {
            // 新增路径：Plugin DAG
            let mut dag = PluginDagRunner::new(
                plugins_config,
                self.plugin_registry.as_ref()
                    .ok_or("plugin_registry not configured")?,
                self.tool_registry.as_ref()
                    .ok_or("tool_registry not configured")?,
            );
            let output = dag.run(input, &self.profile.agent.model)?;
            // schema validation（复用现有逻辑）
            self.validate_with_schema(output, max_turns)
        } else {
            // 现有路径：不变
            self.run_raw(input, max_turns)
        }
    }
}
```

## 8. LlmAgent 微改 (lattice-agent)

### 8.1 set_system_prompt 替换语义

```rust
impl Agent {
    /// 替换已有的 system message（非追加）。
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

### 8.2 PluginAgent::send_message_with_tools（新增）

```rust
impl PluginAgent for Agent {
    fn send_message_with_tools(&mut self, message: &str) -> Result<String, Box<dyn Error>> {
        let events = self.run(message, 10);  // Agent::run 有完整 tool loop
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

## 9. ToolRegistry (lattice-harness)

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
```

Agent 工具执行层集成 ToolRegistry 不在本 spec。本 spec 定义接口，集成延后。

## 10. 内置插件

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

共享工具函数：`builtin/parse_utils.rs`（markdown 清洗、JSON 提取、confidence 提取）。

## 11. 实现顺序

```
第1轮:  ErasedPlugin trait + blanket impl
第2轮:  PluginBundle + PluginMeta + BehaviorMode + to_behavior()
第3轮:  PluginRegistry
第4轮:  Agent.set_system_prompt() 替换语义
第5轮:  PluginAgent::send_message_with_tools() 新增方法
第6轮:  重构 PluginRunner: 提取 run_plugin_loop() → 实现 ErasedPluginRunner
第7轮:  PluginsConfig + PluginSlotConfig + AgentEdgeConfig（profile 扩展）
第8轮:  PluginDagRunner（intra-agent 编排）
第9轮:  AgentRunner 集成（检测 [plugins] → PluginDagRunner）
第10轮: ToolRegistry + ToolError + merge_tool_definitions
第11轮: parse_utils + 3 个内置插件（Refactor, TestGen, SecurityAudit）
第12轮: 剩余 6 个内置插件
第13轮: 测试 + 集成 + 文档
```

## 12. 与现有代码的关系

### 不动

- `lattice-core`: 全部
- `lattice-agent`: AgentState, Memory trait, 7 个内置工具
- `lattice-harness`: Pipeline, HandoffRule, HandoffTarget, HandoffCondition, eval_rules, AgentRegistry, EventBus, Watcher, WebSocket, dry_run
- `lattice-plugin`: Behavior trait, StrictBehavior, YoloBehavior, PluginHooks, PluginConfig, RunResult, PluginError, Action, ErrorAction

### 修改

- `lattice-plugin/src/lib.rs`: Plugin trait 加 `output_schema()` 默认方法
- `lattice-plugin/src/runner.rs`: 重构 — 提取 `run_plugin_loop()` 共享函数；`PluginRunner` 委托给它
- `lattice-agent/src/agent.rs`: 加 `set_system_prompt()`
- `lattice-agent/src/lib.rs`: PluginAgent trait 加 `send_message_with_tools()`
- `lattice-harness/src/profile.rs`: AgentProfile 加 `plugins` 字段 + PluginsConfig 等类型
- `lattice-harness/src/runner.rs`: AgentRunner 加 plugin_registry/tool_registry 字段 + run() 分支
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`

### 新增

- `lattice-plugin/src/erased.rs`: ErasedPlugin trait + blanket impl
- `lattice-plugin/src/registry.rs`: PluginRegistry
- `lattice-plugin/src/bundle.rs`: PluginBundle, PluginMeta, BehaviorMode
- `lattice-plugin/src/erased_runner.rs`: ErasedPluginRunner
- `lattice-plugin/src/builtin/`: 9 个插件 + parse_utils.rs + mod.rs
- `lattice-harness/src/dag_runner.rs`: PluginDagRunner
- `lattice-harness/src/tools.rs`: ToolRegistry, ToolError

## 13. 不在此 spec

- Swarm 去中心化编排
- Python 胶水层 + pip 分发
- 运行时动态加载 (.wasm / .so)
- intra-agent Fork（用 Pipeline Fork 替代）
- Agent 工具执行层重构（ToolRegistry 集成延后）
