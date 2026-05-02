# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.2.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness` + `lattice-agent`（微改）

## 设计目标

- Plugin = 原子 LLM 推理能力（定义 Input/Output/prompt 转换）
- DagAgent = 多个 Plugin 的 DAG 编排 + 共享工具集 + handoff 路由
- 分层：Plugin 内部 Behavior 管 Done/Retry/Escalate，外部 HandoffRule 管跨 Plugin 路由

## 架构分层

```
lattice-harness    DagAgent + PluginSlot + AgentEdge + ForkTarget + ToolRegistry
                   复用 HandoffRule / HandoffTarget / Pipeline
lattice-plugin     Plugin trait + ErasedPlugin + PluginRegistry + 内置插件
lattice-agent      LlmAgent（send / submit_tools / 对话状态），不感知 Plugin
                   + 微改：set_system_prompt() 替换而非追加
lattice-core       模型路由 + 推理，不动
```

## Plugin trait 设计

### 类型安全 Plugin（插件作者实现）

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

相比现有 `Plugin` trait，唯一新增方法是 `output_schema()`（返回 JSON Schema，给下游 slot 解析用）。

### 类型擦除 ErasedPlugin（进 PluginRegistry 用）

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

// blanket impl：serde_json 做中间格式
impl<T: Plugin> ErasedPlugin for T
where
    T::Input: DeserializeOwned,
    T::Output: Serialize,
{
    fn to_prompt_json(&self, input: &serde_json::Value) -> Result<String, PluginError> {
        let typed: T::Input = serde_json::from_value(input.clone())
            .map_err(|e| PluginError::Parse(e.to_string()))?;
        Ok(self.to_prompt(&typed))
    }

    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let typed = self.parse_output(raw)?;
        serde_json::to_value(typed)
            .map_err(|e| PluginError::Parse(e.to_string()))
    }

    // name, system_prompt, tools, preferred_model, output_schema 直接委托
}
```

**约束**: 所有 Plugin 的 `Input` 必须实现 `DeserializeOwned`，`Output` 必须实现 `Serialize`（现有 trait bounds 已满足）。

### Behavior（不变）

```rust
pub trait Behavior: Send + Sync {
    fn decide(&self, confidence: f64) -> Action;       // Done | Retry
    fn on_error(&self, error: &PluginError, attempt: u32) -> ErrorAction; // Retry | Abort | Escalate
}
```

## PluginBundle + PluginRegistry

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
    pub default_behavior: BehaviorMode,
    pub default_tools: Vec<ToolDefinition>,
}

pub enum BehaviorMode {
    Strict {
        confidence_threshold: f64,
        max_retries: u32,
        escalate_to: Option<String>,   // 补充了缺失字段
    },
    Yolo,
}

impl BehaviorMode {
    /// 转换为 trait object 供 PluginRunner 使用
    pub fn into_behavior(self) -> Box<dyn Behavior> {
        match self {
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
    pub fn names(&self) -> Vec<&str>;
    pub fn len(&self) -> usize;
}
```

## DagAgent DAG 模型

### 命名说明

`lattice_agent::Agent` 和本 crate 新增 `DagAgent` 不冲突。harness 代码中：

```rust
use lattice_agent::Agent as LlmAgent;
use crate::agent::DagAgent;
```

### 核心类型

```rust
pub struct DagAgent {
    pub name: String,
    pub model: String,                       // 默认模型，slot 可覆盖
    pub entry: String,                       // 入口 slot 名
    pub slots: HashMap<String, PluginSlot>,  // HashMap 足够，沿 edges 遍历
    pub edges: Vec<AgentEdge>,              // DAG 边
    pub shared_tools: Vec<ToolDefinition>,
    pub shared_memory: Option<Arc<dyn Memory>>,
    pub max_total_turns: u32,
}

pub struct PluginSlot {
    pub plugin_name: String,                 // 引用 PluginRegistry
    pub behavior: BehaviorMode,
    pub tools: Vec<ToolDefinition>,          // slot 专属工具（追加到 shared + plugin.tools()）
    pub model_override: Option<String>,
    pub max_turns: u32,                      // slot 内 LLM 最大调用次数
}

/// 复用 HandoffRule 完整逻辑：condition + all + any + default。
/// 不拆散成 condition + to 两个字段，直接用 HandoffRule。
pub struct AgentEdge {
    pub from: String,                        // 源 slot 名
    pub rule: HandoffRule,                   // 条件 + target，复用现有
}

/// Fork 子流程完成后 merge 到的目标 slot。
pub struct ForkTarget {
    pub branches: Vec<String>,               // 并行 slot 名
    pub merge_to: String,                    // merge 后继续执行的 slot
}
```

AgentEdge 中的 `HandoffRule`：
- `rule.target` → 下一个 slot（Single）或 fork 分支（Fork）
- `rule.condition` / `rule.all` / `rule.any` / `rule.default` → 匹配条件
- `rule.target = None` → 管线终点

### 编排模式

```
顺序链      A ──→ B ──→ C                      AgentEdge { from: A, rule: { default, target: B } }
条件分支    A ──┬──→ B  (conf>0.7)              两条 AgentEdge：
               └──→ C  (else)                    { from: A, rule: { condition: conf>0.7, target: B } }
                                                  { from: A, rule: { default, target: C } }
并行        A ──→ fork:[B,C] → merge → D         AgentEdge { from: A, rule: { target: ForkTarget { branches: [B,C], merge_to: D } } }
           合并输出 {B: out_b, C: out_c} → D
```

### DagAgent::run 执行流程

```
DagAgent.run(input, registry)
  current_name = entry
  input_json = serde_json::json!({"input": input})  // 初始输入包成 JSON

  for turn in 0..max_total_turns:
    slot = slots[current_name]
    plugin_bundle = registry.get(slot.plugin_name)

    // === 每个 slot 重建 LlmAgent，避免 system prompt 累积污染 ===
    let model = slot.model_override.unwrap_or(self.model)
    let resolved = resolve(&model)?
    let mut llm = LlmAgent::new(resolved)
    llm.set_system_prompt(plugin_bundle.plugin.system_prompt())  // 新 Agent，只有一条 system msg

    // 合并三层工具
    let tools = merge_tools(&self.shared_tools, &slot.tools, plugin_bundle.plugin.tools())
    llm = llm.with_tools(tools)  // builder 模式，一次性设置

    // === PluginRunner 内循环 ===
    let behavior = slot.behavior.into_behavior()
    loop:  // behavior.decide() 决定 Done/Retry
      prompt = plugin_bundle.plugin.to_prompt_json(&input_json)?
      raw = llm.send(&prompt)?              // 内部走 tool loop: send → tool_call → submit_tools → ...
      output_json = plugin_bundle.plugin.parse_output_json(&raw)?
      confidence = extract_confidence(&raw)

      match behavior.decide(confidence) {
        Done => break,
        Retry => continue (with backoff),
      }

    // === 找下一个 slot ===
    let matching_edge = self.edges.iter()
      .find(|e| e.from == current_name && e.rule.eval(&output_json))

    match matching_edge.and_then(|e| e.rule.target.clone()) {
      Some(HandoffTarget::Single(next)) => {
        current_name = next
        input_json = output_json    // 上一个输出 = 下一个输入
      }
      Some(HandoffTarget::Fork(names)) => {
        // 并行：std::thread::spawn 每个 branch
        let merged = run_fork(&names, &output_json)  // → {B: out, C: out}
        // 找到对应 ForkTarget 的 merge_to
        let merge_to = find_fork_merge_to(&names)?
        current_name = merge_to
        input_json = merged
      }
      None => return Ok(output_json)  // DAG 终点
    }

  Err: max_total_turns exceeded
```

**关键设计决定**：每个 slot 重建 `LlmAgent`。代价是一次 `resolve()` 调用（内部 OnceLock 缓存，零开销）和一次 HTTP client clone。收益：system prompt 不累积污染，tools 不泄漏，无状态 bug。

## Fork 路由模型

Fork 产生合并输出 `{branch_a: output_a, branch_b: output_b}`。合并后的 JSON 作为 `merge_to` slot 的输入。merge_to slot 的 AgentEdge 条件基于合并 JSON 评估。

```
当前 slot: code-review
Edge: { from: "code-review", rule: { target: ForkTarget { branches: ["security","perf"], merge_to: "summarize" } } }

1. code-review 输出 → match edge → Fork(["security","perf"])
2. 并行执行 security(输出) + perf(输出)
3. merge: {"security": security_output, "perf": perf_output}
4. current = "summarize", input = merge_json
5. summarize slot 的 AgentEdge 条件在 merge_json 上评估
   e.g. field = "security.vulnerabilities[any].severity"
```

## ToolRegistry + MCP 适配

```rust
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
}

enum ToolEntry {
    Native(Arc<dyn Fn(serde_json::Value) -> Result<String, String> + Send + Sync>),
    McpBacked { server: String, tool_name: String },
}

impl ToolRegistry {
    pub fn register_native(&mut self, name: &str, handler: ...);
    pub fn register_mcp(&mut self, server: &str, tool_name: &str);
    pub fn dispatch(&self, tool_name: &str, args: serde_json::Value) -> Result<String, String>;
}

/// 合并三层工具：shared (基础) + slot (覆盖) + plugin (最终覆盖)
/// 同名工具：plugin 优先 > slot > shared
fn merge_tools(
    shared: &[ToolDefinition],
    slot: &[ToolDefinition],
    plugin: &[ToolDefinition],
) -> Vec<ToolDefinition>;
```

- MCP 工具通过 `ToolRegistry::register_mcp(server, tool_name)` 接入
- Plugin 的 `tools()` 返回 ToolDefinition（描述），执行体由 ToolRegistry dispatch

## 内置插件列表

| 插件 | Input | Output | 说明 |
|------|-------|--------|------|
| `CodeReview` | `CodeReviewInput { diff, file_path, context_rules }` | `CodeReviewOutput { issues: Vec<Issue>, confidence }` | 已有 |
| `Refactor` | `RefactorInput { code, issues: Vec<Issue>, instructions }` | `RefactorOutput { refactored_code, changes: Vec<Change> }` | 重构 |
| `TestGen` | `TestGenInput { code, language, focus_areas }` | `TestGenOutput { tests: String, coverage_estimate: f64 }` | 测试生成 |
| `SecurityAudit` | `SecurityAuditInput { code, dependencies, threat_model }` | `SecurityAuditOutput { vulnerabilities: Vec<Vuln>, risk_score: f64 }` | 安全审计 |
| `DocGen` | `DocGenInput { code, doc_type, audience }` | `DocGenOutput { documentation: String, sections: Vec<String> }` | 文档生成 |
| `PptxGen` | `PptxGenInput { topic, outline, template }` | `PptxGenOutput { slides: Vec<Slide>, speaker_notes }` | PPT 生成 |
| `DeepResearch` | `DeepResearchInput { query, sources, depth }` | `DeepResearchOutput { findings: Vec<Finding>, citations, confidence }` | 深度研究 |
| `ImageGen` | `ImageGenInput { prompt, style, dimensions }` | `ImageGenOutput { image_url, alt_text, metadata }` | 图片生成 |
| `KnowledgeBase` | `KnowledgeBaseInput { query, kb_sources }` | `KnowledgeBaseOutput { results: Vec<Result>, relevance_scores }` | 外部知识库 |

每个插件的 Input/Output 是独立 Rust struct，放在 `lattice-plugin/src/builtin/<name>/types.rs`。

## Handoff 集成

两层路由，各司其职：

```
PluginRunner 内:  Behavior::decide(confidence) → Done | Retry
                 Behavior::on_error(error, attempt) → Retry | Abort | Escalate

DagAgent DAG:    AgentEdge.rule.eval(output) → 决定下一个 PluginSlot
                 复用 HandoffRule 完整逻辑 (condition/all/any/default + target)
```

## 与现有代码的关系

### 不动

- `lattice-core`: 所有模块
- `lattice-agent`: 大部分不动（仅加 `set_system_prompt` 替换语义）
- `lattice-harness`: HandoffRule, HandoffTarget, HandoffCondition, Pipeline, AgentRunner, AgentProfile, AgentRegistry
- `lattice-plugin`: Plugin trait 接口（仅加 `output_schema()`），Behavior trait, PluginRunner, PluginHooks, PluginConfig, CodeReviewPlugin

### 新增

- `lattice-plugin/src/erased.rs`: ErasedPlugin trait + blanket impl
- `lattice-plugin/src/registry.rs`: PluginRegistry
- `lattice-plugin/src/bundle.rs`: PluginBundle, PluginMeta, BehaviorMode
- `lattice-plugin/src/builtin/`: refactor, test_gen, security_audit, doc_gen, pptx_gen, deep_research, image_gen, knowledge_base（每插件一个文件 + types.rs）
- `lattice-harness/src/agent.rs`: DagAgent struct + run loop + run_fork
- `lattice-harness/src/plugin_slot.rs`: PluginSlot
- `lattice-harness/src/agent_edge.rs`: AgentEdge + ForkTarget
- `lattice-harness/src/tools.rs`: ToolRegistry + merge_tools + MCP client adapter

### 修改

- `lattice-plugin/src/lib.rs`: `pub use erased::ErasedPlugin;` + `pub mod registry; pub mod bundle; pub mod builtin;`，Plugin trait 加 `output_schema()`
- `lattice-plugin/Cargo.toml`: 可能加 serde_json（已有）
- `lattice-harness/src/lib.rs`: 加 `pub mod agent; pub mod plugin_slot; pub mod agent_edge; pub mod tools;`
- `lattice-harness/Cargo.toml`: 加 `lattice-plugin`
- `lattice-agent/src/agent.rs`: Agent 加 `set_system_prompt(&mut self, prompt: &str)` — 替换已有 system message，而不是追加

## 实现顺序

```
第1轮: ErasedPlugin + PluginBundle + PluginRegistry (lattice-plugin)
第2轮: BehaviorMode + into_behavior() (lattice-plugin)
第3轮: Agent::set_system_prompt() 替换语义 (lattice-agent)
第4轮: DagAgent + PluginSlot + AgentEdge + ForkTarget (lattice-harness)
第5轮: DagAgent::run loop (串联 harness 和 plugin，每 slot 重建 LlmAgent)
第6轮: ToolRegistry + merge_tools + MCP client adapter (lattice-harness)
第7轮: Fork 并行执行 + merge (复用现有 std::thread::spawn)
第8轮: 3 个内置插件 (Refactor, TestGen, SecurityAudit)
第9轮: 剩余 5 个内置插件
第10轮: 测试 + 集成 + 文档
```

## 不在此 spec

- Swarm 去中心化编排（保留未来）
- Python 胶水层 + pip 分发（Phase 7）
- 运行时动态加载 (.wasm / .so)
- 结构化输出 schema::<T>()
- MCP Gateway（Plugin 作为 MCP Server 暴露给外部）
