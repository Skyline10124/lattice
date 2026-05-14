# lattice-orchestrator API

`lattice-orchestrator` is the profile and pipeline layer. It replaces the older documentation name `lattice-bus`.

## Main Types

- `AgentProfile`: TOML-backed profile with agent, system, tools, behavior, handoff, memory and plugin configuration.
- `AgentFactory`: assembles a resolved model, profile, memory, tools and prompt providers into an `AgentRunner`.
- `AgentRunner`: executes one profile-backed agent and validates structured output when configured.
- `Pipeline`: runs handoff and fork flows across registered profiles.
- `AgentRegistry`: loads and stores profiles by name.
- `EventBus` and `PipelineEvent`: broadcast pipeline execution events.
- `MicroAgent` and `MicroAgentHandle`: RPC/pub-sub micro-agent runtime.
- `Orchestrator` trait and in-memory implementation: local registration, discovery, RPC and pub/sub.

## Pipeline Construction

```rust
let registry = AgentRegistry::load_dir(agents_dir)?;
let mut pipeline = Pipeline::new("review", registry, None, Some(event_bus))
    .with_plugin_registry(plugin_registry)
    .with_tool_registry(tool_registry)
    .with_credentials(credentials);

let run = pipeline.run("start-agent", "input").await;
```

## Agent Assembly

`assembly.rs` is the canonical profile-backed construction path. It resolves the effective tool definitions from profile config and `ToolRegistry`, wires prompt providers, memory, hooks and the `DefaultToolExecutor`, then returns an `AgentRunner`.

## Configuration Files

`lattice_dir.rs` discovers local/global LATTICE config directories and `OrchestratorToml`. Profile files are loaded through `AgentProfile::load()` and can be checked with `Pipeline::dry_run()`.
