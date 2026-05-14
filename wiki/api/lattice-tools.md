# lattice-tools API

`lattice-tools` owns built-in tool schemas, capability selection and default execution.

## ToolExecutor

```rust
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> String;
}
```

## DefaultToolExecutor

`DefaultToolExecutor` executes built-in tools and optional registry-backed tools:

- `workspace_context`
- `read_file`, `read_files`
- `grep`, `list_directory`, `find_files`
- `write_file`, `patch`, `multi_patch`
- `bash`
- `todo_write`
- `web_search`
- `bus:fetch` when the blob-store feature is enabled

Execution is constrained by `lattice-sandbox::SandboxConfig`.

## Capability Selection

```rust
pub enum ToolPreset {
    Minimal,
    Reader,
    CodeAgent,
}

pub enum ToolBundle {
    Core,
    Workspace,
    Filesystem,
    Editing,
    Shell,
    Planning,
    Web,
}
```

`resolve_capabilities()` maps presets and bundles to tool names and `ToolDefinition` values. `AgentSpec` and profile-backed assembly use this as the policy layer for what a model can see.

## ToolRegistry

`ToolRegistry` registers custom native handlers with their `ToolDefinition`. The registry is used by orchestration and plugin DAG paths to merge plugin/default/tool selections.
