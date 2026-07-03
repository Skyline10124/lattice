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

> **Drift note:** `docs/LANCET.md` documents a `[lancet.isolation]` profile
> section, landlock, and netns command isolation. Those were **removed**
> (commit `a29072e2`); there is no `LancetIsolationSection`,
> `landlock`, `required_boundaries`, or `netns` in
> `crates/lattice-profile/src/schema.rs`. This document reflects the current
> schema and omits removed isolation features.

---

## 1. Configuration Overview

LATTICE configuration is driven by two mechanisms:

1. **`agent.toml` agent profiles** — the single strict profile file that
   defines agent identity, the system prompt, enabled CEP tools, the LLM
   retry policy, the Upper microVM workspace, and LANCET execution policy.
2. **Environment variables** — LLM provider API keys, the OpenAI-compatible
   gateway endpoint/model, logging level, the global agents directory, and
   Upper VM resource fallbacks.

### Where configuration lives

Profile discovery is owned by `crates/lattice-profile/src/discovery.rs`. A
profile is resolved from a `ProfileSource`, which is one of:

| `ProfileSource` | Meaning |
|---|---|
| `Path(PathBuf)` | An exact filesystem path to an `agent.toml`. |
| `Name(String)` | A named agent looked up project-local, then user-global. |
| `Default` | The `default` agent (equivalent to `Name("default")`). |

Lookup order: **explicit path > project-local > user-global**. The discovery
paths are:

| Location | Path |
|---|---|
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
|---|---|---|---|
| `schema_version` | yes | `u64` | Must be `1`. |
| `[agent]` | yes | `AgentSection` | Agent identity (name, model, optional description). |
| `[prompt]` | yes | `PromptSection` | System prompt — inline `system` or file `system_path`. |
| `[tools]` | no (defaults empty) | `ToolsSection` | Explicitly enabled CEP tool method names. |
| `[llm]` | no (defaults) | `LlmSection` | Provider stream-open retry policy (`[llm.retry]`). |
| `[lancet]` | no (defaults) | `LancetSection` | LANCET execution policy sub-tables. |
| `[workspace]` | **yes (mandatory)** | `WorkspaceSection` | Upper microVM execution environment. |

Every turn unconditionally starts a microVM (MSB/Upper); profiles without
`[workspace]` fail to parse.

### `[lancet]` sub-sections

The `[lancet]` table has a `mode` field plus typed sub-tables. Unknown scalar
fields under `[lancet]` are rejected; unrecognized sub-tables are preserved in
an `extensions` map. The real sub-sections are:

| Sub-section | Struct | Purpose |
|---|---|---|
| `mode` | `CepMode` | Effect-region enforcement mode. |
| `[lancet.project_root]` | `LancetProjectRootSection` | Effective workspace scope for observation. |
| `[lancet.observation]` | `LancetObservationSection` | Path allow-roots and observation/todo budget limits. |
| `[lancet.network]` | `LancetNetworkSection` | `fetch.web` host policy and response size cap. |
| `[lancet.command]` | `LancetCommandSection` | Command execution policy overrides. |
| `[lancet.policy]` | `LancetPolicySection` | Pointer to an external structured TOML policy file. |
| `[lancet.effects]` | `LancetEffectsSection` | Runtime effect-kind deny switch. |
| `[lancet.workspace]` | `LancetWorkspacePolicySection` | Immutable write-deny policy (blacklist). |
| `[lancet.verify]` | `LancetVerifySection` | Direct post-mutation verify command. |

> **Removed:** `[lancet.isolation]` does not exist. There is no
> `LancetIsolationSection`, landlock, `required_boundaries`, or netns in the
> profile schema.

### CEP method catalog (tool names)

`[tools].enabled` and `[lancet]` policy operate over the production CEP
catalog (`lancet-catalog::CATALOG`), which contains exactly **6**
agent-visible methods:

| Method | Impact |
|---|---|
| `observe.workspace` | `workspace_observation` |
| `mutate.create` | `workspace_mutation` |
| `mutate.edit` | `workspace_mutation` |
| `mutate.batch` | `workspace_mutation` |
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
|---|---|---|---|
| `name` | `String` | *(required)* | Agent name. |
| `model` | `String` | *(required)* | Model alias (resolved by the router; see Model Aliases). |
| `description` | `Option<String>` | `None` | Optional human description. |

### `[prompt]` — `PromptSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `system` | `Option<String>` | `None` | Inline system prompt. |
| `system_path` | `Option<String>` | `None` | Path to a system prompt file. Use `system` **or** `system_path`, not both. |

### `[tools]` — `ToolsSection`

| Field | Type | Default | Description |
|---|---|---|---|
| `enabled` | `Vec<String>` | `[]` | Explicit list of enabled CEP method names (dotted, e.g. `"session.todo_write"`). No `bundles`. |

### `[llm.retry]` — `LlmRetrySection`

The `[llm]` section only contains `[llm.retry]`.

| Field | Type | Default | Description |
|---|---|---|---|
| `max_retries` | `u32` | `3` | Max retries when opening a provider stream. |
| `initial_delay_ms` | `u64` | `500` | Initial backoff delay in ms. |
| `max_delay_ms` | `u64` | `3000` | Maximum backoff delay in ms. |

### `[workspace]` — `WorkspaceSection` (mandatory)

| Field | Type | Default | Description |
|---|---|---|---|
| `egress` | `Option<EgressPolicyConfig>` | `None` → `open` | Upper VM egress policy: `open`, `blocked`, or `allowlist`. |
| `credentials` | `Vec<CredentialRef>` | `[]` | Credential refs injected at turn time (env var names only; secrets never stored). |
| `image` | `Option<String>` | `None` | OCI image ref or pre-warmed template name. Optional at schema level; required at runtime. |
| `cpus` | `Option<u32>` | provisioner default | Virtual CPUs for the Upper VM. |
| `memory_mb` | `Option<u32>` | provisioner default | Memory in MiB for the Upper VM. |
| `allowed_egress_targets` | `Vec<String>` | `[]` | Domain names permitted when `egress = "allowlist"`. Ignored otherwise. |
| `cleanup_rounds` | `u32` | `3` | Agent cleanup rounds before budget exhaustion. Range `1..=32`. |
| `max_pending_snapshots` | `u32` | `5` | Max pending retained snapshots per session. Range `1..=100`. |

`CredentialRef`:

| Field | Type | Description |
|---|---|---|
| `env_var_name` | `String` | Name of the env var holding the secret (e.g. `"OPENAI_API_KEY"`). The resolved value is injected at turn time and never stored in the profile. |

`EgressPolicyConfig` (snake_case): `open` (full public internet; default),
`blocked` (deny all outbound), `allowlist` (deny all except
`allowed_egress_targets`).

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
|---|---|---|---|
| `allowed_roots` | `Vec<String>` | `[project_root]` | Canonicalized path allow-roots; each must be inside the effective project root. |
| `max_single_read_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max bytes per single read. |
| `max_turn_observation_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max observation bytes per turn. |
| `max_grep_matches` | `Option<usize>` | `1000` | Max grep matches. |
| `max_list_entries` | `Option<usize>` | `1000` | Max list entries. |
| `max_find_paths` | `Option<usize>` | `1000` | Max find paths. |
| `max_todo_ops` | `Option<usize>` | `100` | Max `todo_write` operations per turn. |

### `[lancet.network]` — `LancetNetworkSection`

| Field | Type | Default (authority) | Description |
|---|---|---|---|
| `allow_public_https` | `Option<bool>` | `true` | Allow public HTTPS `fetch.web`. |
| `allow_localhost` | `Option<bool>` | `false` | Allow localhost `fetch.web`. |
| `allowed_hosts` | `Vec<String>` | `[]` | Explicitly allowed hosts (overrides defaults when non-empty). |
| `max_web_response_bytes` | `Option<usize>` | `10485760` (10 MiB) | Max `fetch.web` response bytes. |

### `[lancet.command]` — `LancetCommandSection`

All fields are `Option`; when `None`, `CommandPolicy::defaults()` applies.

| Field | Type | Default (authority) | Description |
|---|---|---|---|
| `default_timeout_ms` | `Option<u64>` | `120000` | Default command timeout in ms. |
| `max_timeout_ms` | `Option<u64>` | `300000` | Maximum allowed timeout in ms. |
| `max_output_bytes` | `Option<usize>` | `200000` | Max output bytes preserved per stream. |
| `allow_inline_shell` | `Option<bool>` | `false` | Allow inline shell (`sh -c`, `bash -c`, etc.). |
| `inline_shell_max_script_bytes` | `Option<usize>` | `4096` | Max inline-shell script length in bytes. |
| `inline_shell_max_commands` | `Option<usize>` | `16` | Max simple commands in one inline-shell script. |
| `inline_shell_max_words_per_command` | `Option<usize>` | `64` | Max words per inline-shell simple command. |
| `allowed_diagnostic_commands` | `Option<Vec<Vec<String>>>` | `cargo test/check/clippy/doc/fmt`, `git diff/status/log/show`, `pwd`, `echo`, `which`, `date`, `uname` | Token-prefix matched diagnostic commands. |
| `denied_commands` | `Option<Vec<Vec<String>>>` | `sh -c`, `bash -c`, `python -c`, `python3 -c`, `node -e`, `eval`, `exec` | Always-denied commands (highest precedence). |
| `allowed_env` | `Option<Vec<String>>` | `RUST_LOG`, `CARGO_TERM_COLOR`, `CARGO_TARGET_DIR`, `RUST_BACKTRACE` | Env keys allowed from caller params. |
| `inherited_env` | `Option<Vec<String>>` | `PATH` | Env keys inherited from the parent process. |

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

### `[lancet.workspace]` — `LancetWorkspacePolicySection`

| Field | Type | Default | Description |
|---|---|---|---|
| `denied_write_patterns` | `Vec<String>` | `[]` | Write-deny glob/exact patterns (blacklist). A path is allowed unless it matches any denied pattern. No system-level denies are hardcoded. Patterns must be non-empty, project-root-relative, and contain no `..` or NUL. |

Glob patterns (e.g. `".git/**"`) deny a directory subtree; exact paths deny a
single file. Generated templates default to `[".git/**"]`.

### `[lancet.verify]` — `LancetVerifySection`

When the `[lancet.verify]` section is present, `argv` must be non-empty and
`timeout_ms` (if set) must be positive.

| Field | Type | Default | Description |
|---|---|---|---|
| `argv` | `Vec<String>` | *(required if section present)* | Verify command argv. |
| `cwd` | `Option<String>` | project root | Workspace-relative working directory. |
| `timeout_ms` | `Option<u64>` | `None` | Verify timeout in ms (must be positive if set). |
| `expected_exit_code` | `Option<i32>` | `0` | Expected exit code (translated default `0`). |

---

## 4. Environment Variables

### LLM provider credentials and endpoints

These are read by `crates/lattice-llm` (router/client) and at turn time by the
Upper provisioner (`crates/lattice-api/src/turn/upper.rs`).

| Variable | Used by | Description |
|---|---|---|
| `ANTHROPIC_API_KEY` | Anthropic aliases (`opus`, `sonnet`, `haiku`) | Anthropic API key. |
| `OPENAI_API_KEY` | OpenAI aliases (`gpt-5.5`, `gpt-5.4`, `gpt-5.3-codex`, `gpt-4o-mini`) | OpenAI API key. |
| `GEMINI_API_KEY` | `gemini` alias | Google Gemini API key. |
| `OPENAI_COMPATIBLE_API_KEY` | `openai-compatible` / `compat:<model>` / `compatible:<model>` / `openai-compatible:<model>` | OpenAI-compatible gateway API key. |
| `OPENAI_COMPATIBLE_BASE_URL` | OpenAI-compatible aliases | Gateway base URL. Defaults to `http://localhost:11434/v1` (Ollama). <!-- VERIFY: OpenAI-compatible default base URL is Ollama local endpoint http://localhost:11434/v1 --> |
| `OPENAI_COMPATIBLE_MODEL` | bare `openai-compatible` alias | Upstream model name for the bare `openai-compatible` alias. Required unless a `compat:<model>` / `compatible:<model>` / `openai-compatible:<model>` prefix alias is used. |

A credential is reported `Present` only if the env var is set and non-empty;
otherwise `Missing`. Secrets are never stored in profiles — `[workspace]`
credentials hold only env var *names*, and values are resolved and injected at
turn time.

### Provider base URLs and model aliases

The router (`crates/lattice-llm/src/router.rs`) maps aliases to providers. The
default model is `sonnet`.

| Alias | Provider | Upstream model | Base URL |
|---|---|---|---|
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
|---|---|---|---|
| `LATTICE_LOG` | `lattice-llm` logging (`src/logging.rs`) | `error` (or `debug` with `--verbose`) | Tracing filter. Takes priority over the `--verbose` flag. Supports `RUST_LOG`-style syntax (e.g. `trace`, `lattice_llm=debug`). |
| `LATTICE_AGENTS_DIR` | `lattice-cli` (`commands/run.rs`) | `$HOME/.lattice/agents` | Overrides the user-global agents directory. Validated against system paths and `/tmp`. |
| `UPPER_DEFAULT_CPUS` | `lattice-workspace` (`microsandbox.rs`) | `1` | Fallback vCPU count for the Upper VM when `[workspace].cpus` is unset. |
| `UPPER_DEFAULT_MEMORY_MB` | `lattice-workspace` (`microsandbox.rs`) | `512` | Fallback memory (MiB) for the Upper VM when `[workspace].memory_mb` is unset. |

> **Note on `LATTICE_LINUX_ISOLATION_LIVE`:** this variable appears in test
> harnesses, `FIX.md`, `docs/LANCET.md`, and release scripts, but is **not**
> read by any runtime source under `crates/`. It is a test-gate flag for live
> Linux isolation canaries, not a configuration variable, and is omitted from
> the configuration surface. Landlock/netns command isolation were removed
> from the profile schema; do not document them as active configuration.

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
    "mutate.batch",
    "fetch.web",
    "session.todo_write",
]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000

[workspace]
# Required at runtime: set to a fully-qualified OCI image or template name.
image = "docker.io/library/python:3.12-slim"
egress = "allowlist"
allowed_egress_targets = ["api.anthropic.com", "pypi.org", "files.pythonhosted.org"]
cpus = 2
memory_mb = 1024
cleanup_rounds = 3
max_pending_snapshots = 5

[[workspace.credentials]]
env_var_name = "ANTHROPIC_API_KEY"

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

[lancet.command]
default_timeout_ms = 120000
max_timeout_ms = 300000
max_output_bytes = 200000
allow_inline_shell = false
inline_shell_max_script_bytes = 4096
inline_shell_max_commands = 16
inline_shell_max_words_per_command = 64
allowed_diagnostic_commands = [
    ["cargo", "test"],
    ["cargo", "check"],
    ["cargo", "clippy"],
    ["cargo", "fmt", "--check"],
    ["git", "diff"],
    ["git", "status"],
]
denied_commands = [
    ["sh", "-c"],
    ["bash", "-c"],
]
allowed_env = ["RUST_LOG", "CARGO_TARGET_DIR", "RUST_BACKTRACE"]
inherited_env = ["PATH"]

[lancet.policy]
# path = "policy.toml"  # optional external policy file (relative, inside project root)

[lancet.effects]
deny = []

[lancet.workspace]
denied_write_patterns = [".git/**"]

[lancet.verify]
argv = ["cargo", "test", "--no-run"]
# cwd = "."             # defaults to project root
# timeout_ms = 120000   # must be positive if set
# expected_exit_code = 0
```

### Minimal valid profile

The strict schema requires `schema_version`, `[agent]`, `[prompt]`, and
`[workspace]`. The `[tools]`, `[llm]`, and `[lancet]` sections default to
empty/defaults. A minimal profile:

```toml
schema_version = 1

[agent]
name = "default"
model = "sonnet"

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

[workspace]
# Set image before running. [lancet.workspace] is optional but recommended.
[lancet.workspace]
denied_write_patterns = [".git/**"]
```

### Profile → policy translation

At turn time, `crates/lattice-api/src/turn/cep.rs` translates the `[lancet]`
sections into LANCET-native config:

- `cep_policy_from_profile` — builds `CepPolicyConfig` (project root,
  observation, network, budget, command, denied effect kinds).
- `command_policy_from_profile` — lowers `[lancet.command]` into
  `CommandPolicy`, falling back to `CommandPolicy::defaults()` for missing
  fields or the whole section.
- `workspace_policy_from_profile` — lowers `[lancet.workspace]` into
  `WorkspacePolicy` and validates deny patterns.
- `verify_config_from_profile` — lowers `[lancet.verify]` into
  `VerifyConfig` (argv required, timeout positive, cwd defaults to project
  root, expected exit code defaults to `0`).
- `load_policy_config` — assembles the unified `PolicyConfig` and, if
  `[lancet.policy].path` is set, loads and merges the external TOML policy
  file.

`[lancet.project_root].root` and `[lancet.observation].allowed_roots` entries
are canonicalized and must resolve inside the runtime project root; otherwise
the turn fails fast with a `BadRequest` error.
