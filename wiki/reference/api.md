# API 参考

本页是当前 workspace 的高层 API 索引。更细的 crate notes 见 [api](../api/README.md)。

## `lattice-core`

底层模型路由和 provider transport：

```rust
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError>;
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<impl Stream<Item = StreamEvent>, LatticeError>;
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, LatticeError>;
```

核心类型：

- `ResolvedModel`
- `Message`, `Role`
- `ToolDefinition`, `ToolCall`, `FunctionCall`
- `StreamEvent`
- `ChatResponse`
- `LatticeError`

## `lattice-prompt`

Prompt assembly：

- `PromptRegistry`
- `ContextProvider`
- `ProviderFactory`
- `SystemPromptStack`
- `SystemPromptDelta`
- `ToolSchemaProvider`
- `ProjectInstructionsProvider`

编译流程：

```text
Collect -> Sort -> Budget -> Trim -> Render
```

## `lattice-sandbox`

执行策略：

- `SandboxConfig`
- `LandlockConfig`
- `CommandHook`
- `AuditSink`
- `SandboxError`

主要检查：

- `check_read`
- `check_write`
- `check_command`
- `check_url`

## `lattice-tools`

工具 schema、能力和执行：

- `ToolExecutor`
- `DefaultToolExecutor`
- `ToolRegistry`
- `RegistryToolAccess`
- `ToolPreset`
- `ToolBundle`
- `CapabilitySelection`
- `resolve_capabilities`
- `default_tool_definitions`

内置工具包括 workspace context、文件读取、搜索、编辑、bash、todo、web fetch 等。

## `lattice-agent`

Agent loop：

```rust
pub struct Agent;

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self;
    pub fn with_tools(self, tools: Vec<ToolDefinition>) -> Self;
    pub fn with_tool_executor(self, executor: Box<dyn ToolExecutor>) -> Self;
    pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent>;
    pub async fn run_streaming<F>(&mut self, content: &str, max_turns: u32, emit: F) -> Vec<LoopEvent>
    where
        F: FnMut(LoopEvent);
}
```

`LoopEvent` covers token output, reasoning output, tool call requests, tool results, turn manifests, done and error events.

## `lattice-plugin`

Plugin runtime:

- `Plugin`
- `ErasedPlugin`
- `PluginBundle`
- `PluginRegistry`
- `PluginRunner`
- `ErasedPluginRunner`
- `PluginDagRunner`
- `PluginsConfig`, `PluginSlotConfig`, `DagEdgeConfig`

Handoff rules are provided by `lattice-core::handoff` and re-exported for plugin DAG configuration.

## `lattice-orchestrator`

Profile and pipeline orchestration:

- `AgentProfile`
- `AgentRegistry`
- `AgentFactory`
- `AgentRunner`
- `Pipeline`
- `PipelineRun`
- `DryRunReport`
- `EventBus`, `PipelineEvent`
- `MicroAgent`, `MicroAgentHandle`
- `Orchestrator`

Use this layer for TOML-backed multi-agent pipelines.

## `lattice-runtime`

Application-facing facade:

- `Runtime`
- `RuntimeBuilder`
- `AgentSpec`
- `BuiltAgent`
- `RuntimeSecurity`
- `RuntimeSecurityConfig`
- `RuntimeHandle`

Use `Runtime` when building applications or CLI/TUI paths that need shared model routing, registries, session store and security policy.

## `lattice-session`

Conversation persistence:

- `Session`
- `SessionMessage`
- `SessionStore`
- `FileSessionStore`
- `SqliteCasSessionStore`
- `TurnManifestObject`
- artifact, ref, branch, tag, stash and GC models

## Historical Names

- `lattice-bus` is now represented by `lattice-orchestrator`.
- `lattice-python` is not present in the current workspace.
- `lattice-registry` is not present; registration/discovery lives in `lattice-orchestrator`.
