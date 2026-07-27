<!-- generated-by: gsd-doc-writer -->
# LATTICE — Getting Started

This guide takes you from a fresh clone to your first committed LLM turn in
LATTICE, a session-centric LLM runtime where every turn is persisted to a
git-backed session graph and all tool execution flows through the LANCET
Controlled Execution Protocol (CEP).

All commands and paths below are verified against the repository source. Claims
that could not be confirmed from the repo are marked with
`<!-- VERIFY: ... -->`.

## 1. Prerequisites

- **Rust >= 1.88** (edition 2024). The workspace pins `rust-version = "1.88"`
  and `edition = "2024"` in the root `Cargo.toml` (`[workspace.package]`). Use
  [rustup](https://rustup.rs/) to install or update a compatible toolchain.
- **git** — the session graph is git-backed, and the workspace builds against
  `git2`.
- **A provider API key** — at least one of the LLM provider credentials below
  must be set in your environment (or a `.env` file at the repo root, which the
  CLI loads via `dotenvy` on startup):
  - `ANTHROPIC_API_KEY` (for the `opus`, `sonnet`, `haiku` aliases)
  - `OPENAI_API_KEY` (for the `gpt-5.5`, `gpt-5.4`, `gpt-5.3-codex`,
    `gpt-4o-mini` aliases)
  - `GEMINI_API_KEY` (for the `gemini` alias)
  - `OPENAI_COMPATIBLE_API_KEY` plus `OPENAI_COMPATIBLE_BASE_URL` and
    `OPENAI_COMPATIBLE_MODEL` (for an OpenAI-compatible gateway, e.g. Ollama)

  These env vars are read by `crates/lattice-llm/src/router.rs` and
  `crates/lattice-llm/src/client.rs`. The default model alias is `sonnet`
  (Anthropic), so `ANTHROPIC_API_KEY` is the shortest path to a working turn.

## 2. Installation

LATTICE is a Cargo workspace of 19 crates. Clone the repository and build it
with cargo.

1. Clone the repo:

   ```bash
   git clone <REMOTE_URL> lattice
   <!-- VERIFY: replace <REMOTE_URL> with the real git remote. The repo's
        configured remotes include github.com:Skyline10124/lattice.git and
        codeberg.org/AstRin/LATTICE.git, but the canonical public clone URL is
        not determinable from the repo alone. -->
   cd lattice
   ```

2. Build the workspace (debug):

   ```bash
   cargo build
   ```

   For an optimized release build:

   ```bash
   cargo build --release
   ```

3. (Optional) Install the `lattice` binary to `$CARGO_HOME/bin`:

   ```bash
   cargo install --path crates/lattice-cli
   ```

   The CLI binary is named `lattice` and is declared in
   `crates/lattice-cli/Cargo.toml` (`[[bin]] name = "lattice"`). Until installed,
   run it via `cargo run -p lattice-cli -- <command>`.

## 3. First run

The shortest path to a committed turn: set a provider key, create a session
bound to an agent profile and project branch, then send a prompt.

1. Export a provider API key (Anthropic is used by the default `sonnet` alias):

   ```bash
   export ANTHROPIC_API_KEY="sk-ant-..."
   ```

2. Create a new session bound to the `default` agent profile on branch `main`:

   ```bash
   cargo run -p lattice-cli -- session new --agent default --project-branch main
   ```

   `session new` requires both `--agent <name>` and `--project-branch <branch>`
   (verified in `crates/lattice-cli/src/commands/session.rs`, `SessionAction::New`).
   It prints the new session id to stdout.

3. Send a prompt to that session. Replace `<session_id>` with the id from the
   previous step:

   ```bash
   cargo run -p lattice-cli -- session send <session_id> "Summarize this repo in one line."
   ```

   The turn streams assistant tokens to stdout and tool/status lines to stderr,
   then records the turn as a commit in the git-backed session graph. A
   successful turn exits with code `0`; a provider/credential/configuration
   error exits with code `2` (verified in
   `crates/lattice-cli/src/commands/run.rs`, exit-code section).

4. (Optional) Open an interactive inline REPL over the session:

   ```bash
   cargo run -p lattice-cli -- tui <session_id>
   ```

To confirm your environment is wired up before sending a real prompt, run the
diagnostics command:

```bash
cargo run -p lattice-cli -- doctor
```

`doctor` reports config load status, per-credential `Present`/`Missing` status,
session-store availability, and the resolved model alias count (verified in
`crates/lattice-cli/src/commands/doctor.rs`). You can also list only the models
with live credentials:

```bash
cargo run -p lattice-cli -- models --auth
```

## 4. Common setup issues

### (a) Missing provider API key

If the required provider credential is unset or empty, turn execution fails
with a credential error. `crates/lattice-llm/src/client.rs`
(`validate_and_read_key`) returns an `LlmError::Credential` when the resolved
`api_key_env` is missing or empty, and the CLI surfaces this as exit code `2`.

Fix it by exporting the env var for the alias you are using:

```bash
# Anthropic (opus, sonnet, haiku)
export ANTHROPIC_API_KEY="sk-ant-..."

# OpenAI (gpt-5.5, gpt-5.4, gpt-5.3-codex, gpt-4o-mini)
export OPENAI_API_KEY="sk-..."

# Gemini (gemini)
export GEMINI_API_KEY="..."

# OpenAI-compatible gateway (openai-compatible, compat:<model>)
export OPENAI_COMPATIBLE_API_KEY="..."
export OPENAI_COMPATIBLE_BASE_URL="http://localhost:11434/v1"
export OPENAI_COMPATIBLE_MODEL="deepseek-chat"
```

Run `cargo run -p lattice-cli -- doctor` or `cargo run -p lattice-cli -- models --auth`
to confirm the credential now reads as `Present`. The CLI also loads a `.env`
file from the current directory on startup via `dotenvy` (in
`crates/lattice-cli/src/main.rs`), so you can place exports there instead of
your shell profile.

### (b) No default agent profile found

The `default` agent profile is resolved by
`crates/lattice-profile/src/discovery.rs` with this lookup order:

1. **Explicit path** (`ProfileSource::Path`) — an exact `agent.toml` path.
2. **Project-local** — `<project_root>/.lattice/agents/<name>/agent.toml`.
3. **User-global** — `~/.lattice/agents/<name>/agent.toml`.

The user-global agents directory is resolved by
`crates/lattice-cli/src/commands/run.rs` (`safe_agents_dir`) in this order:
explicit override path > `LATTICE_AGENTS_DIR` env var > `$HOME/.lattice/agents`
> `.lattice/agents` (relative fallback). Directories inside system paths
(`/etc`, `/usr`, `/var`, `/sys`, `/proc`, `/dev`, `/boot`) or `/tmp` are
rejected as unsafe.

If the `default` profile is missing from both project-local and user-global
locations, discovery returns `ProfileError::MissingDefault` with a message
containing the proposed path and the creation hint
`lattice new agent default` (verified in
`crates/lattice-profile/src/discovery.rs` and `crates/lattice-profile/src/error.rs`).
Discovery never creates files or directories on its own.

To fix it, place an `agent.toml` at one of the discovery paths. A minimal valid
profile (only `schema_version = 1` and the mandatory `[agent]` and `[prompt]`
sections are required; see `crates/lattice-profile/src/schema.rs`,
`AgentProfile`):

```toml
schema_version = 1

[agent]
name = "default"
model = "sonnet"

[prompt]
system = "You are a helpful coding agent."
```

`[tools]`, `[llm]`, and `[lancet]` are optional and default to empty/defaults.
Turns run Direct-Local against the canonical project workspace — no VM image,
credential injection, or `[workspace]` section exists (removed in #40);
legacy profiles that still declare `[workspace]` fail to parse.
### (c) Diagnostics with LATTICE_LOG

If a turn fails or behaves unexpectedly, raise the tracing level for more
detail. `crates/lattice-llm/src/logging.rs` reads `LATTICE_LOG`, which takes
priority over the `--verbose` flag and supports `RUST_LOG`-style syntax:

```bash
export LATTICE_LOG="debug"
# or target a specific crate:
export LATTICE_LOG="lattice_llm=trace,lattice_api=debug"
```

`--verbose` on the CLI is equivalent to `LATTICE_LOG=debug` with timestamps,
file, and line numbers. The LANCET self-check is also useful:

```bash
cargo run -p lattice-cli -- lancet status
```

Operator audit reads (not agent CEP tools) use `lattice audit` after at least
one turn has written the project-keyed audit store:

```bash
cargo run -p lattice-cli -- audit --session <session_id>
```

## 5. Next steps

- **Development workflow** — see [docs/DEVELOPMENT.md](DEVELOPMENT.md).
- **Testing** — see [docs/TESTING.md](TESTING.md).
- **Architecture** — for the layered workspace, CEP method catalog, turn
  execution data flow, and key abstractions, see
  [docs/ARCHITECTURE.md](ARCHITECTURE.md).
- **Configuration** — for the full `agent.toml` schema, all `[lancet]`
  sub-sections, provider base URLs, and the complete environment variable
  reference, see [docs/CONFIGURATION.md](CONFIGURATION.md).

Build, test, and lint commands for contributors:

```bash
cargo build
cargo test
cargo clippy -- -D warnings
cargo fmt --all --check
```
