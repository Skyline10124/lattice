# Issue #46 Slice 2 — Policy-Visible Workspace observation

## Goal

Prove all four `observe.workspace` modes share the same Direct-Local visibility
and bounds through the built binary.

## Fixture matrix

Create a temporary Git project containing:

- tracked `src/lib.rs` and `tracked.txt`;
- non-ignored untracked `notes.txt`;
- ignored `private/ignored.txt` from `.gitignore`;
- explicitly included ignored `private/included.txt`;
- read-denied `denied/hidden.txt`;
- `.git/config`, `.lattice/private.txt`, and an outside-root sentinel.

Profiles vary only `[lancet.workspace]` and `[lancet.observation]`:

```toml
[tools]
enabled = ["observe.workspace"]

[lancet.workspace]
included_ignored_read_patterns = ["private/included.txt"]
denied_read_patterns = ["denied/**"]

[lancet.observation]
max_single_read_bytes = 32
max_grep_matches = 2
max_list_entries = 3
max_find_paths = 3
```

## Tool scripts

Use encoded provider wire names and canonical public assertions:

```json
{"mode":"read_file","path":"tracked.txt","offset":0,"limit":16}
{"mode":"list_directory","path":".","limit":3}
{"mode":"grep","pattern":"needle","path":".","max_results":2}
{"mode":"find_files","pattern":"*.txt","path":".","limit":3}
```

Inspect exact Tool result/error JSON in the next captured provider request;
JSONL intentionally exposes only result size/error summary.

## Required assertions

- tracked and non-ignored untracked paths are visible by default;
- ignored paths are hidden unless explicitly included;
- read denial wins over explicit ignored inclusion;
- `.git`, LATTICE private state, absolute paths, `..`, and outside-root symlink
  paths are denied without content leakage;
- read/list/grep/find agree on visibility for the same path;
- byte/match/entry/path bounds set truncation or deterministic denial;
- profile under `.lattice/agents/<name>` still reads project-root `src/lib.rs`;
- Tool and CEP lifecycle events precede commit and name canonical
  `observe.workspace`;
- audit query records actual path identity, observation impact, capability, and
  verdict without raw secret content.

## Exact replacement IDs

This slice lands `planned_direct_workspace_read_turn`,
`planned_direct_canonical_workspace_root`, `planned_direct_tool_activity_before_commit`,
`planned_direct_activity_commit_order`, and the observation half of
`planned_direct_turn_audit_replay`/`planned_direct_cep_lifecycle`.
Delete mapped internal tests only after the assertion-level superset review.

## Files

- `crates/lattice-system-harness/tests/direct_observation.rs` (new)
- shared harness helpers only where repeated setup is proven
- mapped tests/docs ledger after replacement passes

## Verification

```bash
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo +1.96.0 test -p lattice-system-harness --test direct_observation
cargo +1.96.0 test -p lattice-api usage_run_turn_can_read_workspace_file_with_default_profile
cargo +1.96.0 xtask harness-boundary
```
