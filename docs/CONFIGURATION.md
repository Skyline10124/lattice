<!-- generated-by: gsd-doc-writer -->

# LATTICE Configuration

This document describes how LATTICE is configured: the `agent.toml` agent
profile, profile discovery, LLM provider environment variables, and the
LANCET policy sections that govern tool execution.

All schema details below are grounded in the strict profile schema
(`crates/lattice-profile/src/schema.rs`), profile discovery
(`crates/lattice-profile/src/discovery.rs`), the model router
(`crates/lattice-llm/src/router.rs`, `endpoints.rs`), and the profile→policy
translation (`crates/lattice-api/src/turn/cep.rs`).

> **Removed isolation features:** Landlock/netns command isolation and the
> `[lancet.isolation]` profile section were removed (commit `a29072e2`); there
> is no `LancetIsolationSection`, `landlock`, `required_boundaries`, or `netns`
> in `crates/lattice-profile/src/schema.rs`. See
> [docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed subsystems".

---

## 1. Configuration Overview

LATTICE configuration is driven by two mechanisms:

1. **`agent.toml` agent profiles** — the single strict profile file that
   defines agent identity, the system prompt, enabled CEP tools, the LLM
   retry policy, and LANCET execution policy.
2. **Environment variables** — LLM provider API keys, the OpenAI-compatible
   gateway endpoint/model, logging level, and the global agents directory.

### Where configuration lives

Profile discovery is owned by `crates/lattice-profile/src/discovery.rs`. A
profile is resolved from a `ProfileSource`, which is one of:

| `ProfileSource` | Meaning |
| --- | --- |
| `Path(PathBuf)` | An exact filesystem path to an `agent.toml`. |
| `Name(String)` | A named agent looked up project-local, then user-global. |
| `Default` | The `default` agent (equivalent to `Name("default")`). |

Lookup order: **explicit path > project-local > user-global**. The discovery
paths are:

| Location | Path |
| --- | --- |
| Project-local | `<project_root>/.lattice/agents/<name>/agent.toml` |
| User-global | `~/.lattice/agents/<name>/agent.toml` |

The CLI (`crates/lattice-cli/src/commands/run.rs`) resolves the user-global
agents directory via `safe_agents_dir`:

1. An explicit override path (if provided).
2. `LATTICE_AGENTS_DIR` environment variable.
3. `$HOME/.lattice/agents`.
4. `.lattice/agents` (relative fallback).

The resolved directory is validated against system paths (`/etc`, `/usr`,
`/var`, `/sys`, `/proc`, `/dev`, `/boot`) and `/tmp`; directories inside
those are rejected as unsafe.

When the `default` agent is missing, discovery returns a typed
`ProfileError::MissingDefault` with template text and a creation hint
(`lattice new agent default`). Discovery never creates files or directories.

---

## 2. Configuration File Format / Schema

The profile is a strict TOML file named `agent.toml`. Only
`schema_version = 1` is accepted. The top-level `AgentProfile` struct uses
`#[serde(deny_unknown_fields)]`, so unknown top-level sections are rejected.
Removed sections (`[security]`, `[context]`, `[behavior]`) are explicitly
rejected; `[tools]` only supports `enabled` (no `bundles`).

### Top-level sections

| Section | Required | Struct | Description |
| --- | --- | --- | --- |
| `schema_version` | yes | `u64` | Must be `1`. |
| `[agent]` | yes | `AgentSection` | Agent identity (name, model, optional description). |
| `[prompt]` | yes | `PromptSection` | System prompt — inline `system` or file `system_path`. |
| `[tools]` | no (defaults empty) | `ToolsSection` | Explicitly enabled CEP tool method names. |
| `[llm]` | no (defaults) | `LlmSection` | Provider stream-open retry policy (`[llm.retry]`). |
| `[lancet]` | no (defaults) | `LancetSection` | LANCET execution policy sub-tables. |

The Upper microVM `[workspace]` section was removed in #40: turns run
Direct-Local against the canonical Local Workspace, so the schema neither
declares nor requires `[workspace]`. Legacy profiles that still contain it
fail to parse (the loader names `[workspace]` as a retired section).

### `[lancet]` sub-sections

The `[lancet]` table has a `mode` field plus typed sub-tables. Unknown scalar
fields under `[lancet]` are rejected; unrecognized sub-tables are preserved in
an `extensions` map. The real sub-sections are:

| Sub-section | Struct | Purpose |
| --- | --- | --- |
| `mode` | `CepMode` | Effect-region enforcement mode. |
| `[lancet.project_root]` | `LancetProjectRootSection` | Effective workspace scope for observation. |
| `[lancet.observation]` | `LancetObservationSection` | Path allow-roots and observation/todo budget limits. |
| `[lancet.network]` | `LancetNetworkSection` | `fetch.web` host policy and response size cap. |
| `[lancet.policy]` | `LancetPolicySection` | Pointer to an external structured TOML policy file. |
| `[lancet.effects]` | `LancetEffectsSection` | Runtime effect-kind deny switch (native path only). |
| `[lancet.capabilities]` | `LancetCapabilitiesSection` | Component Tool capability admission policy. |
| `[lancet.workspace]` | `LancetWorkspacePolicySection` | Local Workspace write-deny and read-visibility policy (blacklist). |

> **Removed:** `[lancet.isolation]` does not exist. There is no
> `LancetIsolationSection`, landlock, `required_boundaries`, or netns in the
> profile schema.

### CEP method catalog (tool names)

`[tools].enabled` and `[lancet]` policy operate over the production CEP
catalog (`lancet-catalog::CATALOG`), which contains exactly **5**
agent-visible methods:

| Method | Impact |
| --- | --- |
| `observe.workspace` | `workspace_observation` |
| `mutate.create` | `workspace_mutation` |
| `mutate.edit` | `workspace_mutation` |
| `fetch.web` | `external_observation` |
| `session.todo_write` | `session_state_mutation` |

Audit inspection is **not** a catalog method. Use `lattice audit` (operator CLI)
against the project-keyed audit store; do not list `observe.audit` or
`observe.audit_replay` in `[tools].enabled`.

There is no `session.envelope` method. Scaffold methods (`workspace.inspect`,
`workspace.navigate`) are feature-gated inspection-only and are **not** part of
the production catalog.

---

## 3. All Configuration Options

### `[agent]` — `AgentSection`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `name` | `String` | *(required)* | Agent name. |
| `model` | `String` | *(required)* | Model alias (resolved by the router; see Model Aliases). |
| `description` | `Option<String>` | `None` | Optional human description. |

### `[prompt]` — `PromptSection`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `system` | `Option<String>` | `None` | Inline system prompt. |
| `system_path` | `Option<String>` | `None` | Path to a system prompt file. Use `system` **or** `system_path`, not both. |

### `[tools]` — `ToolsSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | `Vec<String>` | `[]` | Explicit list of enabled CEP method names (dotted, e.g. `"session.todo_write"`). No `bundles`. |

### `[llm.retry]` — `LlmRetrySection`

The `[llm]` section only contains `[llm.retry]`.

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `max_retries` | `u32` | `3` | Max retries when opening a provider stream. |
| `initial_delay_ms` | `u64` | `500` | Initial backoff delay in ms. |
| `max_delay_ms` | `u64` | `3000` | Maximum backoff delay in ms. |

### `[lancet]` top-level

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | `CepMode` | `enforce` | Effect-region enforcement mode. |

`CepMode` (snake_case): `enforce` (reject violations), `warn` (emit warnings),
`off` (skip effect-region checks only; policy, capability, effect-kind denial,
and native routing remain active).

### `[lancet.project_root]` — `LancetProjectRootSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `root` | `Option<String>` | `None` | Effective workspace scope for observation. If set, must resolve inside the runtime project root. |

### `[lancet.observation]` — `LancetObservationSection`

All `max_*` fields are `Option`; when `None`, the authority defaults
(`ObservationPolicyConfig::defaults_for_root`) apply.

| Field | Type | Default (authority) | Description |
| --- | --- | --- | --- |
| `allowed_roots` | `Vec<String>` | `[project_root]` | Canonicalized path allow-roots; each must be inside the effective project root. |
| `max_single_read_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max bytes per single read. |
| `max_turn_observation_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max observation bytes per turn. |
| `max_grep_matches` | `Option<usize>` | `1000` | Max grep matches. |
| `max_list_entries` | `Option<usize>` | `1000` | Max list entries. |
| `max_find_paths` | `Option<usize>` | `1000` | Max find paths. |
| `max_todo_ops` | `Option<usize>` | `100` | Max `todo_write` operations per turn. |

### `[lancet.network]` — `LancetNetworkSection`

| Field | Type | Default (authority) | Description |
| --- | --- | --- | --- |
| `allow_public_https` | `Option<bool>` | `true` | Allow public HTTPS `fetch.web`. |
| `allow_localhost` | `Option<bool>` | `false` | Allow localhost `fetch.web`. |
| `allowed_hosts` | `Vec<String>` | `[]` | Explicitly allowed hosts (overrides defaults when non-empty). |
| `max_web_response_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max `fetch.web` response bytes. |

### `[lancet.policy]` — `LancetPolicySection`

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `Option<String>` | `None` | Path to an external structured TOML policy file. Must be relative, contain no NUL bytes, no `..` components, and resolve inside the runtime project root. |

When present, the external file is loaded and merged via
`PolicyConfig::merge_external`. Merge precedence: **compiled defaults <
profile inline `[lancet.*]` < external policy file < test/runtime override**.

### `[lancet.effects]` — `LancetEffectsSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `deny` | `Vec<ImpactKind>` | `[]` | Runtime effect kinds to deny. |

`ImpactKind` (snake_case): `workspace_observation`, `workspace_mutation`,
`command_execution`, `external_observation`, `session_state_mutation`,
`audit_observation`.

This section is the **native** authorization path only; Component Tool policy
lives in `[lancet.capabilities]` and never names method impact identity.

### `[lancet.capabilities]` — `LancetCapabilitiesSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `deny` | `Vec<String>` | `[]` | Capability names subtracted from the enabled tools' declared set. |
| `max_boundary` | `Trust` | `in` | Highest trust boundary an admitted capability may cross. |

Component Tool admission runs in a fixed stage order: the `[tools].enabled`
tools' declared capabilities form the grant (there is no independent
capability allow-list, so profiles stay portable), `deny` subtracts from it,
`max_boundary` removes every capability whose scope trust resolves to `ex`
(an undeclared scope trust is `ex`), and the concrete per-operation target
check runs at call time. A tool left with no capability is not callable.

`Trust` (snake_case): `in` (the operation stays inside the Tool Host's control
and accounting domain), `ex` (it reaches the external world).

Every `deny` entry must name a capability of the composed Component catalog.
An unknown name is a **hard error at server construction**, never a warning: a
deny typo that silently keeps a grant is fail-open. On a binary whose Component
catalog is empty, *any* `deny` entry therefore fails startup.

### `[lancet.workspace]` — `LancetWorkspacePolicySection`

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `denied_write_patterns` | `Vec<String>` | `[]` | Write-deny glob/exact patterns (blacklist). A path is allowed unless it matches any denied pattern. Patterns must be non-empty, project-root-relative, and contain no `..` or NUL. |
| `included_ignored_read_patterns` | `Vec<String>` | `[]` | Expose specific Git-ignored paths to observation (read/list/grep/find). |
| `denied_read_patterns` | `Vec<String>` | `[]` | Hide paths from observation even when tracked or otherwise visible. |

Glob patterns (e.g. `".git/**"`) deny a directory subtree; exact paths deny a
single file. Generated templates default to `denied_write_patterns = [".git/**"]`.
Read visibility defaults to tracked and non-ignored files, and one visibility
decision applies across read, list, grep, and find. Runtime System Path
Exclusions (project `.git`, LATTICE private state, root escape) always win
over profile rules and are not profile-configurable.

---

## 4. Environment Variables

### LLM provider credentials and endpoints

These are read by `crates/lattice-llm` (router/client). Providers are
selected by the profile's `agent.model` alias at turn time.

| Variable | Used by | Description |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | Anthropic aliases (`opus`, `sonnet`, `haiku`) | Anthropic API key. |
| `OPENAI_API_KEY` | OpenAI aliases (`gpt-5.5`, `gpt-5.4`, `gpt-5.3-codex`, `gpt-4o-mini`) | OpenAI API key. |
| `GEMINI_API_KEY` | `gemini` alias | Google Gemini API key. |
| `OPENAI_COMPATIBLE_API_KEY` | `openai-compatible` / `compat:<model>` / `compatible:<model>` / `openai-compatible:<model>` | OpenAI-compatible gateway API key. |
| `OPENAI_COMPATIBLE_BASE_URL` | OpenAI-compatible aliases | Gateway base URL. Defaults to `http://localhost:11434/v1` (Ollama). <!-- VERIFY: OpenAI-compatible default base URL is Ollama local endpoint http://localhost:11434/v1 --> |
| `OPENAI_COMPATIBLE_MODEL` | bare `openai-compatible` alias | Upstream model name for the bare `openai-compatible` alias. Required unless a `compat:<model>` / `compatible:<model>` / `openai-compatible:<model>` prefix alias is used. |

A credential is reported `Present` only if the env var is set and non-empty;
otherwise `Missing`. Secrets are never stored in profiles — the profile holds
only the model alias, and provider keys are read from the environment at
turn time.

### Provider base URLs and model aliases

The router (`crates/lattice-llm/src/router.rs`) maps aliases to providers. The
default model is `sonnet`.

| Alias | Provider | Upstream model | Base URL |
| --- | --- | --- | --- |
| `gpt-5.5` / `gpt-5` | OpenAI | `gpt-5.5` <!-- VERIFY: alias gpt-5.5 maps to upstream OpenAI model gpt-5.5 --> | `https://api.openai.com/v1` <!-- VERIFY: OpenAI API base URL --> |
| `gpt-5.4` | OpenAI | `gpt-5.4` <!-- VERIFY: alias gpt-5.4 maps to upstream OpenAI model gpt-5.4 --> | `https://api.openai.com/v1` |
| `gpt-5.3-codex` | OpenAI | `gpt-5.3-codex` <!-- VERIFY: alias gpt-5.3-codex maps to upstream OpenAI model gpt-5.3-codex --> | `https://api.openai.com/v1` |
| `gpt-4o-mini` | OpenAI | `gpt-4o-mini` <!-- VERIFY: alias gpt-4o-mini maps to upstream OpenAI model gpt-4o-mini --> | `https://api.openai.com/v1` |
| `opus` | Anthropic | `claude-opus-4-7` <!-- VERIFY: alias opus maps to upstream Anthropic model claude-opus-4-7 --> | `https://api.anthropic.com` <!-- VERIFY: Anthropic API base URL --> |
| `sonnet` | Anthropic | `claude-sonnet-4-6` <!-- VERIFY: alias sonnet maps to upstream Anthropic model claude-sonnet-4-6 --> | `https://api.anthropic.com` |
| `haiku` | Anthropic | `claude-haiku-4-5` <!-- VERIFY: alias haiku maps to upstream Anthropic model claude-haiku-4-5 --> | `https://api.anthropic.com` |
| `gemini` | Gemini | `gemini-2.5-flash` <!-- VERIFY: alias gemini maps to upstream Google model gemini-2.5-flash --> | `https://generativelanguage.googleapis.com/v1beta` <!-- VERIFY: Google Gemini API base URL --> |
| `openai-compatible` | OpenAI-compatible | from `OPENAI_COMPATIBLE_MODEL` | from `OPENAI_COMPATIBLE_BASE_URL` |
| `compat:<m>` / `compatible:<m>` / `openai-compatible:<m>` | OpenAI-compatible | `<m>` | from `OPENAI_COMPATIBLE_BASE_URL` |

Unknown aliases return a typed error — there is no automatic fallback. The
bare `openai-compatible` alias errors if `OPENAI_COMPATIBLE_MODEL` is unset or
empty; a prefix alias (`compat:<model>`) is the alternative.

### LATTICE runtime variables

| Variable | Used by | Default | Description |
| --- | --- | --- | --- |
| `LATTICE_LOG` | `lattice-llm` logging (`src/logging.rs`) | `error` (or `debug` with `--verbose`) | Tracing filter. Takes priority over the `--verbose` flag. Supports `RUST_LOG`-style syntax (e.g. `trace`, `lattice_llm=debug`). |
| `LATTICE_AGENTS_DIR` | `lattice-cli` (`commands/run.rs`) | `$HOME/.lattice/agents` | Overrides the user-global agents directory. Validated against system paths and `/tmp`. |

> **Removed live-isolation flag:** the live Linux isolation canary lane and its
> env flag were removed in #47; the flag has no reader anywhere in the tree
> (not in any `crates/` runtime source). It was never a configuration variable.
> See [docs/ARCHITECTURE.md](./ARCHITECTURE.md) "Removed subsystems".

---

## 5. Example Configuration

A complete, working `agent.toml` using real fields discovered in the schema.
This matches the auto-generated default template
(`lattice-profile::defaults::default_agent_template`) extended with explicit
LANCET policy sections.

```toml
# LATTICE Agent Profile (v1)
schema_version = 1

[agent]
name = "dev"
model = "sonnet"
description = "Full development agent"

[prompt]
system = "You are a full-stack developer. Read, write, edit, and refactor code. Plan changes before acting. Run shell commands for build, test, lint, and version control. Fetch web docs when needed. Be methodical and explain your reasoning."

[tools]
enabled = [
    "observe.workspace",
    "mutate.create",
    "mutate.edit",
    "fetch.web",
    "session.todo_write",
]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000

[lancet]
mode = "enforce"

[lancet.project_root]
root = "."

[lancet.observation]
allowed_roots = ["."]
max_single_read_bytes = 10485760
max_turn_observation_bytes = 10485760
max_grep_matches = 1000
max_list_entries = 1000
max_find_paths = 1000
max_todo_ops = 100

[lancet.network]
allow_public_https = true
allow_localhost = false
allowed_hosts = ["docs.rs", "crates.io"]
max_web_response_bytes = 10485760

[lancet.policy]
# path = "policy.toml"  # optional external policy file (relative, inside project root)

[lancet.effects]
deny = []

[lancet.capabilities]
deny = []
max_boundary = "in"

[lancet.workspace]
denied_write_patterns = [".git/**"]
included_ignored_read_patterns = []
denied_read_patterns = []
```

### Minimal valid profile

The strict schema requires `schema_version`, `[agent]`, and `[prompt]`. The
`[tools]`, `[llm]`, and `[lancet]` sections default to empty/defaults. A
minimal profile:

```toml
schema_version = 1

[agent]
name = "default"
model = "sonnet"

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

# Optional but recommended.
[lancet.workspace]
denied_write_patterns = [".git/**"]
```

### Profile → policy translation

At turn time, `crates/lattice-api/src/turn/cep.rs` translates the `[lancet]`
sections into LANCET-native config:

- `cep_policy_from_profile` — builds `CepPolicyConfig` (project root,
  observation, network, budget, denied effect kinds).
- `workspace_policy_from_profile` — lowers `[lancet.workspace]` into
  `WorkspacePolicy` and validates deny/read-visibility patterns.
- `load_policy_config` — assembles the unified `PolicyConfig` and, if
  `[lancet.policy].path` is set, loads and merges the external TOML policy
  file.
- `AgentProfile::capability_admission_policy` — lowers `[tools].enabled` plus
  `[lancet.capabilities]` into the `CapabilityAdmissionPolicy` that gates
  Component Tools. Without a profile nothing is admitted (fail closed).
`[lancet.project_root].root` and `[lancet.observation].allowed_roots` entries
are canonicalized and must resolve inside the runtime project root; otherwise
the turn fails fast with a `BadRequest` error.
