# LATTICE vs Claude Code vs Codex Gap Analysis

**Date:** 2026-05-08
**Scope:** Compare local LATTICE framework against Claude Code source at `~/Programs/claude-code-source-code` and Codex source at `~/Programs/codex`.
**Question:** 和 Claude Code、Codex 对比，我们的框架差在哪里？

## Executive Summary

LATTICE 的差距不是“没有框架抽象”。相反，LATTICE 已经有 model router、agent loop、plugin runtime、pipeline/orchestrator、sandbox、memory、TUI 雏形，框架骨架比很多项目更完整。

真正差距在三个层面：

1. **编码代理产品闭环不完整** - Claude Code 和 Codex 都围绕真实编码任务打磨了工具、权限、上下文、会话、TUI、恢复、审批、测试和部署体验。LATTICE 目前更像底层 engine。
2. **上下文和长期任务能力弱** - LATTICE 有 prompt section budget，但缺少成熟的 compaction、AGENTS/CLAUDE 指令层级、文件状态缓存、会话恢复语义、技能按需注入和任务状态系统。
3. **安全/权限和工具语义不够产品化** - LATTICE 有 allowlist、敏感文件、Landlock、审计，但缺少解释型审批、持久化规则、跨平台 sandbox 后端、工具级安全语义、PTY/持续命令、结构化 patch。

一句话：LATTICE 现在像“可嵌入的多模型 agent runtime”，但还不是 Claude Code/Codex 这种“能长期可靠改代码的本地编码代理产品”。

## Evidence Snapshot

| Area | LATTICE | Claude Code source | Codex source |
|------|---------|--------------------|--------------|
| Scale | 10 Rust crates, about 175 Rust files under `crates/` | about 1,884 TS/TSX files under `src/` | about 97 workspace directories, about 1,842 Rust files |
| Tests | about 24 Rust test entry files | source dump is analysis-oriented, not directly comparable | about 784 Rust test files or test modules |
| Tool system | built-ins: read, grep, write, list, bash, patch, web_search, bus:fetch | 40+ tools with validate, permissions, render, progress, concurrency metadata | typed handlers for shell, unified exec, apply_patch, MCP, multi-agents, goals, image, request input, plugin install |
| Context | prompt sections with token budgets, drops over-budget sections | compact system, CLAUDE.md, memory, file history, session logs, skills | context manager, compaction, AGENTS.md, skill injection, permissions and environment context |
| Permissions | static allowlists plus sensitive file checks | permission context, hooks, tool-specific checks, UI prompts | permission profiles, approval requests, saved prefix rules, filesystem/network policies |
| Product surface | CLI/TUI skeleton, run/code/tui commands | REPL, slash commands, resume UI, tasks, teams, MCP, bridge/remote | TUI, exec, app server, cloud tasks, MCP client/server, IDE/app integration |

## Phase 14 Eval Anchors

Phase 14 uses this gap analysis as the research anchor for deterministic evals covering dirty worktree safety, project instructions, targeted reading, patch recovery, verification honesty, and profile isolation.

## Core Gap 1: Product Harness

### What competitors have

Claude Code wraps the basic agent loop with:

- Slash commands and REPL workflow.
- Plan mode, todo/task tools, background tasks, subagents and teams.
- Session JSONL logs, resume/fork session, file history snapshots.
- MCP lifecycle, OAuth/config, dynamic tool registration.
- Per-tool React/Ink rendering and progress messages.
- Feature flags and runtime gates for staged rollout.

Codex wraps the loop with:

- Full TUI and headless `exec`.
- App server/protocol crates for IDE/app/cloud integrations.
- Rollout/thread/session store.
- MCP client and server.
- Agent graph, multi-agent handlers, goal handlers.
- Unified exec with PTY sessions and stdin continuation.

### What LATTICE has

LATTICE has the core execution stack:

- `lattice-core`: transport, streaming, router, catalog.
- `lattice-agent`: agent loop, prompt registry, memory, tool execution.
- `lattice-plugin`: plugin trait, typed runner, DAG runner.
- `lattice-orchestrator`: pipeline, handoff, micro-agent bus.
- `lattice-cli`: `run`, `code`, `tui`, sessions, models, resolve, doctor.

### Gap

LATTICE lacks the production harness around the loop:

- No mature command palette/slash command system.
- No plan/todo/task state integrated into the model loop.
- No background command/session lifecycle.
- No interactive permission UX.
- No robust resume/fork model with tool call state and file snapshots.
- No polished TUI flow for day-to-day coding.

Priority: high. This is the biggest difference users feel.

## Core Gap 2: Tool Semantics

### LATTICE current state

`DefaultToolExecutor` supports a small fixed set: `read_file`, `grep`, `write_file`, `list_directory`, `bash`, `patch`, `web_search`, and optionally `bus:fetch`.

The `patch` tool is search/replace based:

- It requires exact `search` text.
- It errors if the text appears more than once.
- It does not model add/delete/update hunks as a formal patch grammar.
- It does not provide a mature diff/apply verification path.

### Claude Code / Codex state

Claude Code has tool objects with lifecycle methods:

- input validation
- permission checks
- execution
- progress rendering
- grouped rendering
- concurrency safety
- read-only/destructive metadata
- tool-specific prompts and API mapping

Codex has typed handlers and spec tests for:

- unified exec with PTY and `write_stdin`
- `apply_patch` grammar and parser
- request user input
- request permissions
- tool search
- MCP resource tools
- multi-agent tools
- goal/plan tools

### Gap

LATTICE tools are useful primitives, but not yet a product-grade coding tool layer.

Missing pieces:

- Structured patch grammar and verified application.
- PTY command sessions, stdin continuation, polling.
- Output truncation with clear policies.
- Tool specs tested as contracts.
- Tool-specific permission explanations.
- Tool progress events suitable for TUI.
- Dynamic/deferred tool discovery.
- Notebook/image/browser/LSP-class tools if coding-agent parity is the target.

Priority: high.

## Core Gap 3: Context Management

### LATTICE current state

LATTICE has a prompt compiler with layers and token budgets. It sorts sections and drops sections that exceed budget. It also has memory providers and session persistence.

This is a good start, but it is lossy in a crude way: when budget pressure rises, lower-priority context can disappear entirely.

### Competitor state

Claude Code has:

- compact boundary model
- auto compact / snip compact / context collapse strategies
- CLAUDE.md memory loading
- file history snapshots
- lazy skill/memory injection
- JSONL session logs and resume reconstruction

Codex has:

- context manager history normalization
- compaction tasks
- AGENTS.md injection
- environment, permissions, plugins, skills, user instructions as structured context fragments
- truncation policies and token accounting tests

### Gap

LATTICE needs a real long-running context strategy:

- Compaction summary as a first-class turn item.
- Budget pressure handling that summarizes before dropping.
- Hierarchical project instructions such as `AGENTS.md`.
- File state snapshots or at least changed-file summaries.
- Skill/plugin/context injection rules with precedence.
- Turn history normalization after tool calls.

Priority: high.

## Core Gap 4: Permissions and Sandbox

### LATTICE current state

LATTICE has:

- read/write allowlists
- command allowlists
- sensitive file blocking
- max read/write/HTTP sizes
- command timeout
- audit sink
- Landlock on Linux
- hook support

This is meaningful, but mostly static.

### Competitor state

Codex has:

- built-in permission profiles like read-only, workspace-write, danger/no sandbox
- custom filesystem and network policies
- request-permission tools
- sandbox escalation with justification
- saved approval prefix rules
- macOS Seatbelt, Linux Landlock/bubblewrap, Windows sandbox support
- tool schemas that expose permission escalation fields

Claude Code has:

- permission context with allow/deny/ask rules
- per-tool checks
- UI prompts
- hooks before tool use
- additional working directories
- plan/default/bypass modes

### Gap

LATTICE needs interactive, explainable, persistent permission workflows:

- User approval requests from tool calls.
- Persistent but scoped approvals.
- Prefix-rule command approval.
- Network policy beyond URL scheme checks.
- Cross-platform sandbox story.
- Clear denial reasons surfaced to the model and user.
- Workspace trust model.

Priority: high for coding agent use, medium for library-only use.

## Core Gap 5: Multi-Agent Is Architecture-First, Not Workflow-First

LATTICE has an orchestrator, event bus, handoff rules, micro-agent profiles, plugin DAGs. This is strong architecture.

The gap is workflow ergonomics:

- Claude Code has subagent tools, task boards, teams, worktree isolation, agent-to-agent messaging.
- Codex has multi-agent handlers, agent graph store, spawn/wait/send/close tools.
- LATTICE has pipeline execution but less user-facing affordance for "split this work, run agents, merge results, resume them".

LATTICE is closer to LangGraph-style orchestration than Claude Code-style delegation UX.

Priority: medium-high.

## Core Gap 6: MCP and Extension Ecosystem

LATTICE has plugin and tool registries, plus manifest plugins. That is a strong foundation.

Gaps against CC/Codex:

- MCP client lifecycle is not first-class.
- MCP server mode is not first-class.
- No dynamic MCP tool namespace convention.
- No MCP resource listing/read workflow.
- No OAuth/config flow for MCP servers.
- No plugin marketplace/install/request workflow.
- Skills are bundled through superpowers content, but not a full skill manager with explicit/implicit invocation, dependency prompts, metadata budgets, and install flow.

Priority: medium-high.

## Core Gap 7: State, Sessions, and Recovery

LATTICE now stores sessions as JSON with messages and supports continuation. It has improved restoration for tool calls, reasoning content, and tool results.

Still missing:

- Append-only session logs for crash recovery.
- Turn-level events and progress persistence.
- File history snapshots.
- Fork session semantics.
- Cross-project resume/search.
- Background task recovery.
- Compaction boundaries preserved in history.

Priority: medium.

## Core Gap 8: Quality Gates and Test Depth

Codex has hundreds of targeted tests around:

- permissions
- sandbox policies
- shell spec
- apply_patch
- context manager
- MCP
- config parsing
- TUI flows
- session/history behavior

LATTICE has useful tests but much less contract depth. The risk is not just lower coverage. The bigger issue is that public behavior is not yet frozen into executable specs.

Immediate missing contract tests:

- tool schema stability
- sandbox denial edge cases
- patch application correctness
- prompt compaction/budget behavior
- session resume with tool calls
- permission escalation paths
- TUI event rendering
- plugin manifest compatibility

Priority: high.

## Where LATTICE Is Already Strong

LATTICE should not copy everything blindly.

Strengths:

- Multi-provider model routing is central, not bolted on.
- Rust-native engine with clean crate boundaries.
- Plugin trait and typed plugin runner are good framework primitives.
- Orchestrator/pipeline abstraction is ahead of many coding agents.
- Security primitives exist early: allowlists, sensitive files, audit, Landlock.
- Runtime builder makes embedding plausible.

The strategic opportunity is to keep LATTICE as a model-centric runtime, then build a first-party coding harness on top.

## Recommended Priority Order

### P0 - Decide Product Target

Pick one:

1. **Framework target:** compete with LangGraph/CrewAI/LiteLLM-ish runtimes. Then prioritize APIs, plugins, docs, SDKs, examples, MCP, reliability.
2. **Coding agent target:** compete with Claude Code/Codex. Then prioritize TUI, tools, permissions, sessions, context compaction, patch, task workflow.
3. **Runtime plus first-party coding agent:** keep LATTICE core reusable, but build `lattice code` as the flagship product. This is the best fit for current architecture.

Recommendation: option 3.

### P1 - Build Coding Agent Harness

- Formal tool spec layer with stable schemas and tests.
- Replace search/replace patch with grammar-based `apply_patch`.
- Add PTY exec sessions and `write_stdin`.
- Add interactive permission escalation and saved prefix rules.
- Add plan/todo task tools.
- Upgrade TUI around tool progress, approvals, session resume.

### P2 - Build Long-Running Context System

- Add compaction turn item and summary prompt.
- Preserve compact boundaries in session history.
- Add AGENTS.md-style hierarchical project instruction loading.
- Add file-change snapshots or turn diff summaries.
- Add skill/plugin injection precedence.

### P3 - Build Extension Surface

- MCP client lifecycle.
- MCP resource list/read tools.
- MCP server mode if LATTICE should be embeddable by other agents.
- Skill manager: metadata, invocation, dependencies, installer.
- Plugin install/request workflow.

### P4 - Harden Quality

- Contract tests for all tool schemas.
- Sandbox matrix tests.
- Golden tests for context rendering.
- Session replay tests.
- TUI snapshot tests.
- E2E coding-agent tests using mock model/tool streams.

## Bottom Line

LATTICE 差的不是想法，也不是抽象。差的是“真实编码代理每天使用时需要的那些脏活累活”：

- 工具语义要深。
- 权限体验要可信。
- 上下文不能粗暴丢。
- 会话要能恢复。
- TUI 要能承载长任务。
- 测试要锁住行为。

如果只做 framework，当前方向已经有价值，但需要 API/SDK/文档和生态。如果要对标 Claude Code/Codex，就必须把 `lattice code` 当成核心产品，而不是 runtime 的一个 demo command。
