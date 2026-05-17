#!/usr/bin/env bash
set -eu

usage() {
  cat <<'USAGE'
Usage: scripts/release-boundary-check.sh [--docs]

Checks Phase 11 release boundaries:
  - removed old crate identities in manifests, source, and lockfile
  - removed old crate directories
  - unsupported CLI/TUI files
  - forbidden API migration-home file
  - release documentation drift

With --docs, only README.md, AGENTS.md, CLAUDE.md, and .planning/codebase are
checked for release-facing documentation drift.
USAGE
}

docs_only=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --docs)
      docs_only=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

old_hyphen='lattice-(core|runtime|prompt|sandbox|policy)'
old_underscore='lattice_(core|runtime|prompt|sandbox|policy)'
old_any="${old_hyphen}|${old_underscore}"
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

existing_docs_targets() {
  for doc_target in README.md AGENTS.md CLAUDE.md .planning/codebase; do
    [ -e "$doc_target" ] && printf '%s\n' "$doc_target"
  done
}

check_docs() {
  docs_tmp=$(mktemp)
  targets_tmp=$(mktemp)
  existing_docs_targets > "$targets_tmp"

  if [ ! -s "$targets_tmp" ]; then
    print_group "release docs drift" "no release documentation targets found"
    rm -f "$docs_tmp" "$targets_tmp"
    return
  fi

  xargs -r rg -n --hidden --glob '!target/**' --glob '!.planning/phases/**' \
    "$old_any|sessions list|debug \"test prompt\"|config show|tui -m|--agent|--trace|run-v11|run_v11" \
    < "$targets_tmp" > "$docs_tmp" 2>/dev/null || true

  xargs -r rg -n --hidden --glob '!target/**' --glob '!.planning/phases/**' \
    'plugin|swarm|orchestrat|rich prompt|sandboxed tool execution|approval|policy stack|TUI' \
    < "$targets_tmp" 2>/dev/null \
    | awk 'BEGIN { IGNORECASE=1 } !/future|deferred|out of scope|later|not part of v1\.1|removed|unsupported|beyond v1\.1/' \
    >> "$docs_tmp" || true

  print_group "release docs drift" "$(cat "$docs_tmp")"
  rm -f "$docs_tmp" "$targets_tmp"
}

if [ "$docs_only" = true ]; then
  check_docs
  if [ "$failures" -gt 0 ]; then
    echo
    echo "Boundary check failed: $failures group(s)"
    exit 1
  fi
  echo
  echo "Boundary check passed"
  exit 0
fi

manifest_output=$(
  rg -n --hidden "$old_any" Cargo.toml crates/*/Cargo.toml 2>/dev/null || true
)
print_group "workspace metadata old crate identities" "$manifest_output"

source_output=$(
  rg -n --hidden --glob '!target/**' --glob '!.planning/phases/**' --glob '*/src/**/*.rs' \
    "$old_any" crates 2>/dev/null || true
)
print_group "active crate source old crate identities" "$source_output"

old_crate_dirs=""
for crate_dir in \
  crates/lattice-core \
  crates/lattice-runtime \
  crates/lattice-prompt \
  crates/lattice-sandbox \
  crates/lattice-policy
do
  if [ -e "$crate_dir" ]; then
    old_crate_dirs="${old_crate_dirs}${crate_dir}\n"
  fi
done
print_group "removed old crate directories" "$(printf '%b' "$old_crate_dirs" | sed '/^$/d')"

cli_output=""
for cli_target in \
  crates/lattice-cli/src/frontend/tui \
  crates/lattice-cli/src/commands/run_v11.rs \
  crates/lattice-cli/src/commands/sessions.rs \
  crates/lattice-cli/src/commands/debug.rs \
  crates/lattice-cli/src/commands/config_cmd.rs \
  crates/lattice-cli/src/commands/validate.rs \
  crates/lattice-cli/src/commands/new_agent.rs \
  crates/lattice-cli/src/commands/list_agents.rs \
  crates/lattice-cli/src/commands/stats.rs \
  crates/lattice-cli/src/commands/agent_inventory.rs
do
  if [ -e "$cli_target" ]; then
    cli_output="${cli_output}${cli_target}\n"
  fi
done
print_group "unsupported CLI/TUI files" "$(printf '%b' "$cli_output" | sed '/^$/d')"

api_output=""
if [ -e crates/lattice-api/src/topology.rs ]; then
  api_output="crates/lattice-api/src/topology.rs"
fi
print_group "API migration-home files" "$api_output"

lock_output=""
if [ -f Cargo.lock ]; then
  lock_output=$(rg -n "name = \"${old_hyphen}\"" Cargo.lock 2>/dev/null || true)
else
  lock_output="Cargo.lock missing"
fi
print_group "lockfile old workspace packages" "$lock_output"

check_docs

if [ "$failures" -gt 0 ]; then
  echo
  echo "Boundary check failed: $failures group(s)"
  exit 1
fi

echo
echo "Boundary check passed"
