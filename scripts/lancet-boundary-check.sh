#!/usr/bin/env bash
set -eu

usage() {
  cat <<'USAGE'
Usage: scripts/lancet-boundary-check.sh

LANCET boundary gate:
  - lattice-agent must not depend on lancet-native or lattice-tooling
  - active source must not reintroduce legacy executor bypass symbols

Scans Cargo.toml manifests, crates/*/src/**/*.rs, and crate integration tests.
Ignores .planning/**, target/**, and generated build output.
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

failures=0

print_group() {
  group_name="$1"
  group_output="$2"

  if [ -n "$group_output" ]; then
    failures=$((failures + 1))
    printf '\nFAIL [%s]\n' "$group_name"
    printf '%s\n' "$group_output"
  else
    printf 'PASS [%s]\n' "$group_name"
  fi
}

# Narrow allowlists for symbols that are intentionally kept behind native LANCET
# handlers or test-only projections.
DEFAULT_TOOL_EXECUTOR_ALLOWLIST=(
  'crates/lancet-native/src/executor/'
  'crates/lancet-native/src/legacy_backend.rs'
  'crates/lancet-native/src/lib.rs'
)

DEFAULT_TOOL_DEFINITIONS_ALLOWLIST=(
  'crates/lancet-native/src/tool_definitions.rs'
  'crates/lancet-native/src/lib.rs'
  'crates/lattice-api/tests/usage_turn.rs'
)

COMPOSITE_EXECUTOR_ALLOWLIST=(
  'crates/lancet-native/src/registry.rs'
  'crates/lancet-native/src/transport.rs'
)

filter_allowlist() {
  allowlist_tmp=$(mktemp)
  printf '%s\n' "$@" >"$allowlist_tmp"

  while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    file=${line%%:*}
    allowed=false
    for prefix in "${allowlist[@]}"; do
      case "$file" in
        "$prefix"*) allowed=true; break ;;
      esac
    done
    if [ "$allowed" = false ]; then
      printf '%s\n' "$line"
    fi
  done
  rm -f "$allowlist_tmp"
}

scan_source() {
  pattern="$1"
  grep -RInE \
    --include='*.rs' \
    --exclude-dir=target \
    --exclude-dir=.planning \
    "$pattern" crates/*/src crates/*/tests 2>/dev/null || true
}

# ── Dependency graph (cargo metadata) ────────────────────────────────────────

forbidden_deps=""
if ! command -v jq >/dev/null 2>&1; then
  forbidden_deps="jq is required to parse cargo metadata"
else
  metadata_json=$(cargo metadata --no-deps --format-version 1 2>/dev/null || true)
  if [ -z "$metadata_json" ]; then
    forbidden_deps="cargo metadata --no-deps --format-version 1 failed"
  else
    forbidden_deps=$(
      printf '%s' "$metadata_json" | jq -r '
        .packages[]
        | select(.name == "lattice-agent")
        | .dependencies[]
        | select(.name == "lancet-native" or .name == "lattice-tooling")
        | "lattice-agent -> \(.name)"
      ' 2>/dev/null || true
    )
  fi
fi

manifest_forbidden=$(
  grep -nE 'lancet-native|lattice-tooling' crates/lattice-agent/Cargo.toml 2>/dev/null || true
)

graph_output=""
if [ -n "$forbidden_deps" ]; then
  graph_output="$forbidden_deps"
fi
if [ -n "$manifest_forbidden" ]; then
  if [ -n "$graph_output" ]; then
    graph_output="${graph_output}
${manifest_forbidden}"
  else
    graph_output="$manifest_forbidden"
  fi
fi

print_group "lattice-agent forbidden dependency graph" "$graph_output"

# ── DefaultToolExecutor outside executor internals ───────────────────────────

allowlist=("${DEFAULT_TOOL_EXECUTOR_ALLOWLIST[@]}")
default_tool_executor_hits=$(
  scan_source 'DefaultToolExecutor' \
    | filter_allowlist
)

print_group "DefaultToolExecutor outside allowlisted internals" "$default_tool_executor_hits"

# ── default_tool_definitions outside catalog projection path ─────────────────

allowlist=("${DEFAULT_TOOL_DEFINITIONS_ALLOWLIST[@]}")
default_tool_definitions_hits=$(
  scan_source 'default_tool_definitions' \
    | filter_allowlist
)

print_group "default_tool_definitions outside allowlisted fixtures" "$default_tool_definitions_hits"

# ── Legacy ToolExecutor re-export as agent-facing seam ───────────────────────

tool_executor_reexport_hits=$(
  grep -RInE \
    --include='*.rs' \
    --exclude-dir=target \
    --exclude-dir=.planning \
    'pub use .*ToolExecutor|lattice_tooling::ToolExecutor' \
    crates/lattice-agent 2>/dev/null || true
)

print_group "lattice-agent legacy ToolExecutor re-export" "$tool_executor_reexport_hits"

# ── CompositeExecutor outside registry/server internals ──────────────────────

allowlist=("${COMPOSITE_EXECUTOR_ALLOWLIST[@]}")
composite_executor_hits=$(
  scan_source 'CompositeExecutor' \
    | filter_allowlist
)

print_group "CompositeExecutor outside registry/server internals" "$composite_executor_hits"

# ── lattice-cli production bypasses (direct legacy executor / AgentLoop) ─────

CLI_PRODUCTION_ALLOWLIST=(
  'crates/lattice-cli/src/commands/run_tests.rs'
)

cli_bypass_hits=$(
  grep -RInE \
    --include='*.rs' \
    --exclude='*_tests.rs' \
    --exclude-dir=tests \
    --exclude-dir=target \
    --exclude-dir=.planning \
    'DefaultToolExecutor|AgentLoop::new' \
    crates/lattice-cli/src 2>/dev/null || true
)

allowlist=("${CLI_PRODUCTION_ALLOWLIST[@]}")
cli_bypass_hits=$(
  printf '%s\n' "$cli_bypass_hits" | filter_allowlist
)

print_group "lattice-cli production legacy executor bypass" "$cli_bypass_hits"

# ── Final result ─────────────────────────────────────────────────────────────

if [ "$failures" -gt 0 ]; then
  echo
  echo "LANCET boundary check failed: $failures group(s)"
  exit 1
fi

echo
echo "LANCET boundary check passed"
