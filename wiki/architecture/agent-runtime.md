# Agent Runtime

`lattice-agent` provides the async multi-turn agent loop used by direct callers, plugin DAGs and bus pipelines.

## Loop

```text
user input
  → prompt assembly
  → model stream
  → assistant tokens
  → optional tool call
  → sandbox and tool executor
  → tool result message
  → next model turn
```

The loop stops when the model finishes without tool calls, an error occurs, or the configured turn limit is reached.

## Prompt Compiler

The prompt compiler has five stages:

```text
Collect → Sort → Budget → Trim → Render
```

Prompt sections are layered so that kernel system instructions, rules, tools, memory, events and user input can be reasoned about independently. Compiler failures return `Result` errors instead of panicking.

## Tools and Sandbox

Default tools are exposed through `ToolDefinition` and executed by `DefaultToolExecutor`. The sandbox validates commands and URLs before execution:

- hook chain
- shell metacharacter checks
- environment override checks
- command allowlist
- private/reserved IP checks for network access

Linux builds can use Landlock where configured.

## Memory

Runtime includes in-memory and SQLite-backed memory adapters. Shared memory interfaces live in `lattice-core` so bus and agent layers can exchange memory contracts without cross-layer coupling.
