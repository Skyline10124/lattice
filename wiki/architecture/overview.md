# Runtime Architecture

Runtime is the execution layer of LATTICE. It turns a model name, prompt, agent profile or plugin slot into actual model calls, tool execution and events.

## Shape

```text
lattice-core
  ├── catalog and aliases
  ├── ModelRouter
  ├── provider requests/responses
  ├── protocol transports
  ├── streaming parsers
  ├── retry and error classification
  ├── shared message and tool types
  └── security helpers

lattice-prompt
  ├── prompt sections and layers
  ├── prompt registry
  ├── provider factories
  ├── project instructions provider
  └── tool schema provider

lattice-sandbox
  ├── command allowlist checks
  ├── path and sensitive-file checks
  ├── URL and private/reserved host checks
  ├── audit sink interface
  └── Landlock config

lattice-tools
  ├── built-in tool schemas
  ├── capability presets and bundles
  ├── DefaultToolExecutor
  ├── ToolRegistry
  └── registry-backed custom tools

lattice-agent
  ├── Agent
  ├── tool loop
  ├── memory adapters
  ├── prompt providers
  └── hooks and audit log

lattice-plugin
  ├── Plugin trait
  ├── ErasedPlugin
  ├── PluginRegistry
  ├── manifest loader
  ├── PluginDagRunner
  └── runtime behavior policies

lattice-orchestrator
  ├── AgentProfile
  ├── Pipeline
  ├── AgentRunner
  ├── InMemoryOrchestrator
  ├── micro-agent RPC/pub-sub
  ├── watcher
  └── WebSocket serving

lattice-session
  ├── Session and SessionMessage
  ├── file-backed session store
  ├── SQLite CAS session store
  ├── commit, branch, tag, stash operations
  └── turn manifests and artifacts

lattice-runtime
  ├── Runtime
  ├── RuntimeBuilder
  ├── shared ModelRouter
  ├── shared AgentRegistry
  ├── shared PluginRegistry
  ├── shared ToolRegistry
  ├── shared session store and events
  └── RuntimeHandle
```

## Dependency Rules

Runtime crates follow a one-way dependency direction:

```text
core ← prompt/hooks/sandbox/tools ← agent ← plugin/orchestrator ← runtime ← cli
session ← runtime/cli
plugin ← plugins
```

The arrows point toward the dependency. Lower crates do not know about higher crates.

## Call Flow

Direct model call:

```text
model name
  → ModelRouter::resolve
  → ResolvedModel
  → TransportDispatcher
  → provider request
  → StreamEvent / ChatResponse
```

Agent call:

```text
Agent::run
  → prompt registry collects sections
  → lattice-prompt compiler budgets and renders messages
  → lattice-core chat stream
  → optional tool calls
  → lattice-tools executor and lattice-sandbox checks
  → LoopEvent stream
```

Pipeline call:

```text
Runtime::run_pipeline
  → Runtime-owned AgentRegistry
  → Runtime-built Pipeline
  → AgentRunner or PluginDagRunner
  → handoff rules and fork targets
  → events published to EventBus
```

## Boundaries

Runtime exposes contracts that other repositories consume:

- `lattice::core::types`: messages, roles, tools and behavior modes.
- `lattice::plugin::Plugin`: typed plugin interface.
- `lattice::plugin::registry::PluginRegistry`: runtime registry for official or local plugins.
- `lattice::orchestrator::Pipeline`: profile-driven pipeline execution.
- `lattice::runtime::Runtime`: preferred frontend seam for model, agent and pipeline execution.

Runtime does not expose official plugin implementations.
