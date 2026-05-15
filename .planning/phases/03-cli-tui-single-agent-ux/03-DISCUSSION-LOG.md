# Phase 3: CLI/TUI Single-Agent UX - Discussion Log

> **Audit trail only.** Do not use as input to planning, research, or execution agents.
> Decisions are captured in CONTEXT.md — this log preserves the alternatives considered.

**Date:** 2026-05-15
**Phase:** 3-CLI/TUI Single-Agent UX
**Areas discussed:** CLI main entry and profile selection, `code` command removal, TUI single-agent surface, session/profile snapshot UX, test and cleanup boundaries

---

## CLI Main Entry and Profile Selection

| Option | Description | Selected |
|--------|-------------|----------|
| `run --agent` is the main path | `lattice run --agent <name|path>` uses real profile discovery; omitted `--agent` loads `default`. | yes |
| Keep generated temporary `run` profile plus `--agent` | Preserves current ad hoc prompt behavior but keeps a second non-profile runtime chain. | |
| Split `run` and a new profile command | More explicit but adds a new user surface during a convergence milestone. | |

**User's choice:** `run --agent` is the main path, with no compatibility chain.
**Notes:** User explicitly said not to consider compatibility and to keep only one usable chain. Follow-up clarified that Phase 1 default-profile behavior still applies: missing default prompts in interactive mode and errors in non-interactive mode. User then decided that after interactive default creation the command should continue using the generated template, print an edit hint, and not launch an editor.

---

## `code` Command Removal

| Option | Description | Selected |
|--------|-------------|----------|
| `code` is only an ordinary profile name | Delete `lattice code`; `lattice run --agent code` works only if a normal `code` profile exists. | yes |
| Keep a coding template | Runtime remains generic but template layer still treats coding as semi-special. | |
| Keep hidden `code` fallback behavior | `--agent code` could create or load a special coding profile. | |

**User's choice:** `code` is only an ordinary profile name.
**Notes:** User also chose to delete `coding_agent.rs`, move any useful helper behavior into neutral modules, and keep only tests that prove generic profile runner behavior.

---

## TUI Single-Agent Surface

| Option | Description | Selected |
|--------|-------------|----------|
| Delete plugin commands entirely | Remove `/plugins` and `/plugin` with no replacement compatibility commands. | yes |
| Convert plugin command into agent switching | Add `/agent <name>` and multi-profile session state. | |
| Keep removed-feature hints | Keep plugin commands only to tell users the feature is gone. | |

**User's choice:** Delete plugin commands entirely.
**Notes:** User chose to replace plugin statusline/welcome/composer labels with the current agent/profile label, such as `agent:default`. User also chose to keep one active agent/session in TUI and avoid profile switching or profile session maps in Phase 03.

---

## Session/Profile Snapshot UX

| Option | Description | Selected |
|--------|-------------|----------|
| Session snapshot wins | Restored sessions use the bound immutable profile snapshot; drift and ignored `--agent` are warnings. | yes |
| Re-read current `--agent` file on restore | File edits affect restored sessions immediately. | |
| Error when profile changed | Block restore until the user starts a new session or resolves mismatch. | |

**User's choice:** Session snapshot wins.
**Notes:** User confirmed that `--agent` only affects new sessions. If `--agent` conflicts with a restored session snapshot, ignore it and warn. Warnings are non-blocking: stderr in CLI, system message in TUI, and `/status` should expose agent name plus snapshot hash.

---

## Test and Cleanup Boundaries

| Option | Description | Selected |
|--------|-------------|----------|
| Clean active CLI/TUI surface only | Remove plugin/code/orchestrator UI surfaces from CLI/TUI; leave workspace crate deletion for Phase 4. | yes |
| Delete plugin/orchestrator crates in Phase 03 | More complete but crosses the Phase 4 boundary. | |
| Only hide user-visible help | Lower churn, but leaves obsolete active source paths. | |

**User's choice:** Clean active CLI/TUI surface only.
**Notes:** User said "move on" after the proposed guard list; this was treated as accepting the default guard plan. Guards should fail active CLI/TUI code on `Commands::Code`, `coding_agent`, `code_agent`, active pipeline/orchestrator/plugin command exposure, TUI plugin commands/state/labels, and `RuntimePluginOverlay`, while not failing merely because plugin/orchestrator crates still exist for Phase 4.

---

## the agent's Discretion

- Exact names for neutral helper modules are left to the planner/executor, as long as they use generic runtime/profile terminology.
- Exact warning wording is left open, but must make the snapshot-vs-current-file behavior clear.
- Exact prompt UI for interactive default creation is left open, but it must ask before writing and must not launch an editor.

## Deferred Ideas

- Profile switching/rebind/fork UX.
- Multi-profile session management inside TUI.
- Workspace removal of plugin/orchestrator crates.
- Future coding-oriented templates as normal profile templates rather than runtime special cases.
