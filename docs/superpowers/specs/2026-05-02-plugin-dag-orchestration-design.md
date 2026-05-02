# Plugin DAG 编排系统设计规格

- **Spec 版本**: 0.1.0
- **日期**: 2026-05-02
- **范围**: `lattice-plugin` + `lattice-harness`

## 设计目标

将 Plugin 从"Agent 的特化实现"逆转为"Agent 的原子组成单元"。

- Plugin = 原子 LLM 推理能力（定义 Input/Output/prompt 转换）
- Agent  = 多个 Plugin 的 DAG 编排 + 共享工具集 + handoff 路由
- 分层：Plugin 内部 Behavior 管 Done/Retry/Escalate，外部 HandoffRule 管跨 Plugin 路由

## 架构分层

```
lattice-harness    Agent DAG + PluginSlot + AgentEdge + ToolRegistry
                   复用 HandoffRule / HandoffTarget / Pipeline
lattice-plugin     Plugin trait + ErasedPlugin + PluginRegistry + 内置插件
lattice-agent      Agent 运行时（send / submit_tools / 对话状态），不感知 Plugin
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
    fn output_schema(&self) -> Option<serde_json::Value> { None }
}
```

### 类型擦除（进 PluginRegistry 用）

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

impl<T: Plugin> ErasedPlugin for T { /* blanket impl */ }
```

### Behavior 不变

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
    Strict { confidence_threshold: f64, max_retries: u32 },
    Yolo,
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

## Agent DAG 模型

### 核心类型

```rust
pub struct Agent {
    pub name: String,
    pub model: String,
    pub entry: String,
    pub slots: IndexMap<String, PluginSlot>,
    pub edges: Vec<AgentEdge>,
    pub shared_tools: Vec<ToolDefinition>,
    pub shared_memory: Option<Arc<dyn Memory>>,
    pub max_total_turns: u32,
}

pub struct PluginSlot {
    pub plugin_name: String,
    pub behavior: BehaviorMode,
    pub tools: Vec<ToolDefinition>,
    pub model_override: Option<String>,
    pub max_turns: u32,
}

pub struct AgentEdge {
    pub from: String,
    pub to: HandoffTarget,                        // 复用现有
    pub condition: Option<HandoffCondition>,       // 复用现有，None = 无条件
}
```

### 编排模式

```
顺序链      A ──→ B ──→ C                      (slot A edge → B, slot B edge → C)
条件分支    A ──┬──→ B  (conf>0.7)              (两条 edge，不同 condition)
               └──→ C  (else)
并行        A ──→ fork:B,C                      (HandoffTarget::Fork(["B","C"]))
               B ──→ D                           (B,C 完成后 merge → D)
               C ──┘
```

### Agent::run 执行流程

```
// lattice_agent::Agent 提供 LLM 交互（send + submit_tools + 对话状态）
Agent.run(input, registry)
  current = slots[entry]
  input_text = input
  let llm = lattice_agent::Agent::new(resolve(&self.model)?)  // 模型路由
  loop:
    if turn >= max_total_turns → break
    plugin = registry.get(current.plugin_name)
    tools = merge_tools(shared_tools, current.tools, plugin.tools())
    llm.set_system_prompt(plugin.system_prompt())
    llm.set_tools(tools)

    loop:  // PluginRunner 内循环
      prompt = plugin.to_prompt_json(input)
      raw = llm.send(prompt) → tool calls → submit_tools → ... → final text
      output_json = plugin.parse_output_json(raw)
      confidence = extract_confidence(raw)
      action = behavior.decide(confidence)
      match action {
        Done → break,
        Retry → continue (with backoff),
      }

    edge = edges.find(e.from == current && condition matches output_json)
    match edge.to {
      Single(name) → current = slots[name], input_text = output_json.to_string()
      Fork(names)  → parallel run all fork slots → merge → follow fork_next edge
      None → return output_json (终点)
    }
    turn += 1
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
    pub fn merge(&self, plugin_tools: &[ToolDefinition]) -> Vec<(ToolDefinition, Option<ToolEntry>)>;
    pub fn dispatch(&self, tool_name: &str, args: serde_json::Value) -> Result<String, String>;
}
```

- MCP 工具通过 `ToolRegistry::register_mcp(server, tool_name)` 接入
- Plugin 自己的 `tools()` 返回 ToolDefinition（描述），执行体由 Plugin 实现
- Agent.send() 时合并三层工具：shared_tools (基础) + slot.tools (追加/覆盖) + plugin.tools() (追加/覆盖)
- 同名工具：slot 覆盖 shared，plugin 覆盖 slot（越具体优先级越高）

## 内置插件列表

| 插件 | Input | Output | 说明 |
|------|-------|--------|------|
| `CodeReview` | `{diff, file_path, context}` | `{issues[], confidence}` | 已有 |
| `Refactor` | `{code, review_issues, instructions}` | `{refactored_code, changes[]}` | 重构 |
| `TestGen` | `{code, focus_areas[]}` | `{tests, coverage_estimate}` | 生成测试 |
| `SecurityAudit` | `{code, dependencies[], threat_model}` | `{vulnerabilities[], risk_score}` | 安全审计 |
| `DocGen` | `{code, doc_type, audience}` | `{documentation, sections[]}` | 生成文档 |
| `PptxGen` | `{topic, outline[], template}` | `{slides[], speaker_notes}` | PPT 生成 |
| `DeepResearch` | `{query, sources[], depth}` | `{findings[], citations[], confidence}` | 深度研究 |
| `ImageGen` | `{prompt, style, dimensions}` | `{image_url, alt_text, metadata}` | 图片生成 |
| `KnowledgeBase` | `{query, kb_sources[]}` | `{results[], relevance_scores[]}` | 外置知识库 |

## Handoff 集成

两层路由，各司其职：

```
PluginRunner 内:  Behavior::decide(confidence) → Done | Retry
                 Behavior::on_error(error, attempt) → Retry | Abort | Escalate

Agent DAG:       AgentEdge::condition 匹配 JSON output → 决定下一个 PluginSlot
                 复用 HandoffCondition (field/op/value) + HandoffTarget (Single/Fork)
```

## 与现有代码的关系

### 不动

- `lattice-core`: 所有模块
- `lattice-agent`: Agent struct, AgentState, PluginAgent trait, Memory trait
- `lattice-harness`: HandoffRule, HandoffTarget, HandoffCondition, Pipeline, AgentRunner, AgentProfile, AgentRegistry
- `lattice-plugin`: Plugin trait, Behavior trait, PluginRunner, PluginHooks, PluginConfig, CodeReviewPlugin

### 新增

- `lattice-plugin/src/trait.rs`: ErasedPlugin trait + blanket impl
- `lattice-plugin/src/registry.rs`: PluginRegistry
- `lattice-plugin/src/bundle.rs`: PluginBundle, PluginMeta, BehaviorMode
- `lattice-plugin/src/builtin/`: 8 个新插件
- `lattice-harness/src/agent.rs`: Agent struct + run loop
- `lattice-harness/src/plugin_slot.rs`: PluginSlot
- `lattice-harness/src/agent_edge.rs`: AgentEdge
- `lattice-harness/src/tools.rs`: ToolRegistry + merge_tools

### 修改

- `lattice-plugin/src/lib.rs`: 加 `pub mod registry; pub mod bundle; pub mod builtin;`
- `lattice-plugin/Cargo.toml`: 可能加依赖 (serde_json 已有)
- `lattice-harness/src/lib.rs`: 加 `pub mod agent; pub mod plugin_slot; pub mod agent_edge; pub mod tools;`
- `lattice-harness/Cargo.toml`: 加依赖 `lattice-plugin`

## 实现顺序

```
第1轮: ErasedPlugin + PluginBundle + PluginRegistry (lattice-plugin)
第2轮: Agent + PluginSlot + AgentEdge (lattice-harness)
第3轮: Agent::run loop (串联 harness 和 plugin)
第4轮: ToolRegistry + merge_tools
第5轮: 并行 Fork 支持 (复用现有 run_fork)
第6轮: 3 个内置插件 (Refactor, TestGen, SecurityAudit)
第7轮: 剩余 5 个内置插件
第8轮: 测试 + 集成
```

## 不在此 spec

- Swarm 去中心化编排（保留未来）
- Python 胶水层 + pip 分发（Phase 7）
- 运行时动态加载 (.wasm / .so)
- 结构化输出 schema::<T>()
- MCP Gateway（Plugin 作为 MCP Server 暴露给外部）
