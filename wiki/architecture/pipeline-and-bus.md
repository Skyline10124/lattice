# Pipeline and Orchestrator

`lattice-orchestrator` turns TOML agent profiles into executable pipelines and exposes runtime events through an in-memory orchestrator and event bus.

## Agent Profiles

Profiles describe model selection, system prompt, tools, memory, bus settings, handoff rules and optional plugin DAG slots.

```toml
[agent]
name = "review"
model = "sonnet"

[system]
prompt = "You are a precise reviewer."

[handoff]
fallback = "human"
```

## Pipeline Flow

```text
Pipeline::run
  → AgentRegistry lookup
  → profile validation
  → AgentRunner or PluginDagRunner
  → handoff decision
  → optional fork targets
  → PipelineRun result
```

The pipeline has configurable iteration limits and dry-run validation.

## Bus

The in-memory orchestrator provides:

- agent registration
- request/response RPC
- pub/sub events
- capability checks
- bounded concurrency
- channel buffer sizing separate from RPC concurrency

`InMemoryOrchestrator` is intended for local runtime orchestration. External transports belong at the edge, such as Swarm or the WebSocket entrypoint.

## WebSocket

The orchestrator crate owns the optional WebSocket serving code because it exposes pipeline events and runtime state. Secrets and allowed origins are configurable through environment or orchestrator configuration.
