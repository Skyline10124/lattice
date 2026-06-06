# AGENTS.md

Workspace-level architecture guidance for coding agents. This file intentionally
stays high level; for crate-local ownership, key files, and focused verification,
read `crates/<crate>/AGENTS.md`.

## Architecture

LATTICE is a session-centric LLM runtime. Conversation state lives in a
git-style session graph, public turn execution is exposed through `lattice-api`,
and tool execution flows through LANCET. LANCET uses CEP (Controlled Execution
Protocol) as a JSON-RPC method protocol with capability envelopes, policy
gating, path guards, effect checks, and an audit trail.

The workspace dependency graph is layered. Keep dependencies pointing from
higher layers to lower layers; do not introduce reverse dependencies.

```text
Layer 0 (leaf):  lancet-protocol   lattice-llm   lattice-session
Layer 1:         lancet-catalog(->protocol)
                 lattice-profile(->protocol)
                 lattice-agent(->llm,session)
Layer 2:         lancet-dispatch(->protocol,catalog)
                 lancet-audit(->protocol,catalog)
                 lancet-authority(->protocol,catalog,llm)
                 lancet-invariants(->protocol,catalog)
Layer 2.5:       lancet-cep(->protocol,catalog,dispatch)
Layer 3:         lancet-native(->protocol,catalog,dispatch,audit,
                               authority,invariants,llm)
Layer 3.5:       lancet-api(->cep,protocol,catalog,dispatch,audit,
                            authority,invariants,native)
Layer 4:         lattice-api(-> lancet-api + lattice runtime crates)
Layer 5:         lattice-cli(-> api + presentation dependencies)
```

## Crate Roles

| Crate | Role |
|---|---|
| `lancet-protocol` | Pure CEP wire/data contracts: JSON-RPC frames, execution envelopes, errors, policy hook types, provenance wire types. No I/O and no tokio. |
| `lancet-catalog` | Typed CEP method contracts, params/results, descriptors, static production catalog, schema data, and catalog-declared base authority requirements for projections. |
| `lancet-dispatch` | Generic method registry, typed CEP dispatch, in-process transport, cancellation, and event sinks. |
| `lancet-cep` | Public CEP facade over protocol/catalog/dispatch, plus catalog-derived `ToolDefinition`, `ToolExposure`, catalog-declared base-authority aggregation, and explicit `BaseAuthorityPlan` construction. No native execution, policy side effects, audit persistence, or LATTICE dependencies. |
| `lancet-audit` | CEP audit persistence, query filters, stage/impact labels, provenance projection, and attestation persistence. |
| `lancet-authority` | Policy semantics, capability matching, Guard L1 path/scope checks, active envelopes, effect ledger, and effect-region checks. |
| `lancet-invariants` | Invariant card loading/indexing/review helpers and Guard L2 Rust AST checks. |
| `lancet-native` | Production native handlers, server lifecycle, verify, edit/shell/web helpers, audit integration, and impact analysis. |
| `lancet-api` | Unified LANCET public API. Composes the CEP runtime (`LancetRuntime`/`LancetServer`: server lifecycle, `CepClient`, audit, attestation, catalog-derived tool exposure) from the native crates, taking LANCET-native policy types. Independent of LATTICE session/agent/profile layers. |
| `lattice-llm` | Provider adapters, SSE streaming, model routing, tool-call vocabulary, URL/DNS security, and logging. |
| `lattice-session` | Session graph contract, immutable turn commits, branch refs, HEAD state, and SQLite graph store. |
| `lattice-profile` | Strict `agent.toml` schema, discovery, defaults, and LANCET profile sections. |
| `lattice-agent` | Multi-turn `AgentLoop`, state machine, message assembly, and injectable LLM/tool traits. |
| `lattice-api` | Thin public facade for session creation/opening and turn execution. Translates profile `[lancet]` policy into LANCET-native config and wires the agent loop over `lancet-api`'s CEP runtime. |
| `lattice-cli` | Binary entry point and presentation layer over API/session/model/audit commands. |

## CEP And LANCET Contracts

- `lancet-protocol` is the bottom contract. Keep it data-only and runtime-free.
- `lancet-cep` is the public CEP facade. It may frame/re-export protocol,
  catalog, dispatch, tool exposure, and base-authority planning, but must not
  depend on native runtime, authority policy implementation, audit persistence,
  or any `lattice-*` crate.
- `lancet-catalog::CATALOG` is the authoritative production method catalog.
  It contains exactly these 9 CEP methods: `observe.workspace`, `mutate.create`,
  `mutate.patch`, `exec.command_run`, `fetch.web`, `session.todo_write`,
  `session.envelope`, `observe.audit`, and `observe.audit_replay`.
  Descriptors also declare method-local base-authority requirements; hosts and
  policy decisions are supplied later by LANCET runtime policy, not by catalog.
- LLM-facing CEP tool schemas are generated only from the catalog through
  `lancet-cep::project_tool_definitions`; do not reintroduce native/API shim
  projection functions or parallel hard-coded tool definitions.
- Mutating methods require an active `session.envelope` capability scope and
  must pass authority policy, Guard L1, effect checks, audit recording, and
  optional verification.
- LANCET policy is configured through profile `[lancet]` sections: mode,
  project root, observation, network, command, isolation, and external policy
  file pointers.
- SQLite for CEP audit records belongs in `lancet-audit`; SQLite for
  conversation graph state belongs in `lattice-session`.

## Turn Execution

1. `lattice-api::stream_turn` is the single live execution core.
2. `run_turn` drains the same stream through `RunTurnCollector`.
3. `turn/cep.rs` translates profile `[lancet]` policy into LANCET-native config
   and delegates runtime composition to `lancet_api::LancetRuntime::build`
   (server lifecycle, dispatch client, audit), then wraps the returned
   `CepClient` in a `CepExecutorAdapter`.
4. `AgentLoop` reads session state, assembles messages, calls the LLM through
   `LlmClient`, and sends requested tool calls through the injected executor.
5. Native handlers execute typed CEP methods behind policy, guard, audit,
   invariant, effect, and verification checks.
6. Turn outcomes are persisted as commits in the session graph.

## Review Records

- `FIX.md` is the append-only changelog for applied fixes. Never delete or
  overwrite old entries.
- Add a `FIX.md` entry only after the fix has landed. Include issue id/severity,
  problem, fix summary, verification commands/results, and changed files when
  practical.
- `FINDINGS.md` tracks open, deferred, partial, or residual issues. Every open
  entry should include severity/id, location, problem, current status, and plan.
- When fixing a finding, record the applied fix in `FIX.md` and remove it from
  the open section of `FINDINGS.md` or move it to an obsolete/closed section.
- For partial fixes, record the completed work in `FIX.md` and keep the
  remaining risk in `FINDINGS.md` with an explicit residual status and plan.

## Repository Hygiene

- New Rust source files under `crates/**` must start with the workspace copyright
  and SPDX header:

  ```rust
  // Copyright 2026 Ast Rin
  // SPDX-License-Identifier: Apache-2.0
  ```

- Preserve existing copyright and SPDX headers when editing source files.
- Do not add or alter headers for generated, vendored, or external files unless
  the surrounding maintained source already uses that convention.
- Do not modify `.gitignore` in agent-driven changes. In particular, do not add
  ignore exceptions or whitelist rules to force ignored private/planning/build
  paths into commits. If a path appears to need tracking despite ignore rules,
  stop and ask a human maintainer to change the policy explicitly.

## Ownership Rules

- Start protocol shape changes in `lancet-protocol`, then update catalog,
  authority, native handlers, API mapping, and tests as needed.
- Start CEP method changes in `lancet-catalog`, then update native handlers and
  catalog-derived tool projection.
- Keep provider protocol details in `lattice-llm`; higher layers should consume
  typed events and errors.
- Keep native filesystem, shell, web, mutation, verify, and transport lifecycle
  implementation in `lancet-native`.
- Keep LANCET runtime composition and its public surface in `lancet-api`; it
  takes LANCET-native policy types and must not depend on LATTICE
  session/agent/profile crates. Anything that drives CEP (CLI, tests, embedders)
  should spawn through `lancet_api::LancetServer`/`LancetRuntime`, not hand-roll
  the registry/transport/server dance.
- Keep public session and turn API contracts in `lattice-api`; it owns the
  profile→policy translation and the agent-loop wiring over `lancet-api`. Keep
  CLI output and argument parsing in `lattice-cli`.
- Prefer crate-local `AGENTS.md` files for detailed navigation before editing.
