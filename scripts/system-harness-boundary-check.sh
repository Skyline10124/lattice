#!/usr/bin/env bash
set -eu

usage() {
  cat <<'USAGE'
Usage: scripts/system-harness-boundary-check.sh

Ensures lattice-system-harness does not depend on product crates:
  - no lattice-* or lancet-* package names in Cargo.toml dependency tables
    (resolved through `package =` renames, not just the dependency key)
  - no path dependencies into crates/ (any TOML quote style or rename)
  - no workspace = true inheritance from the product workspace
  - no transitive dependency on any workspace member crate (name-agnostic
    backstop via the cargo `resolve` graph — catches neutral-named product
    crates like net-guard and indirect deps regardless of declaration form)

The manifest is parsed with Python's `tomllib` (stdlib, Python 3.11+) rather
than regex, so all valid TOML forms (single/double quotes, inline tables,
`package =` renames) are handled. `cargo metadata` is the parsed-and-resolved
backstop for the transitive closure.
USAGE
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

manifest="crates/lattice-system-harness/Cargo.toml"
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

# ── Manifest: forbidden deps via real TOML parse ─────────────────────────────
# Parse the manifest with `tomllib` (Python 3.11+ stdlib) instead of regex.
# Regex-based checks are bypassable by valid TOML: single-quoted strings
# (`path = '../net-guard'`), inline-table renames
# (`guard = { package = "net-guard", path = '../net-guard' }`), etc. A real
# parser handles every form. Two sub-checks run from one parse:
#   (a) forbidden product crate NAME — the actual package name, resolved
#       through `package =` when the dep key is a rename (e.g. `guard =`).
#       Flags lattice-*/lancet-* names.
#   (b) forbidden product PATH — any `path` value (in a dep table) that
#       resolves under `repo_root/crates/`, regardless of crate name. This is
#       what catches a neutral-named product crate like net-guard declared as
#       `guard = { package = "net-guard", path = '../net-guard' }`, which
#       bypasses the name check (key is `guard`, name `net-guard` is neutral)
#       AND the metadata check's old lattice-*/lancet-* filter.
# Only dependency tables are inspected, so `[[bin]] path = "src/main.rs"` and
# similar non-dep `path` keys do not false-positive.

manifest_forbidden_deps=$(
  python3 - "$manifest" "$repo_root" <<'PY'
import os, sys, tomllib

manifest = sys.argv[1]
repo_root = os.path.normpath(sys.argv[2])
manifest_dir = os.path.dirname(os.path.abspath(manifest))
crates_dir = os.path.normpath(os.path.join(repo_root, "crates"))

with open(manifest, "rb") as fh:
    data = tomllib.load(fh)

def under_crates_dir(resolved):
    resolved = os.path.normpath(resolved)
    return resolved == crates_dir or resolved.startswith(crates_dir + os.sep)

def iter_dep_tables(table, prefix=""):
    """Yield (table_path, dep_key, dep_value) for every *-dependencies table.

    Cargo dependency tables are keyed by any name ending in `dependencies`
    (e.g. `dependencies`, `dev-dependencies`, `build-dependencies`,
    `target.<triple>.dependencies`, `target.<cfg>.dev-dependencies`). Nested
    target tables are walked recursively.
    """
    for key, value in table.items():
        full = f"{prefix}.{key}" if prefix else key
        if isinstance(value, dict):
            # A table named *dependencies: its direct children are deps.
            if key.endswith("dependencies"):
                for dep_key, dep_val in value.items():
                    yield full, dep_key, dep_val
            else:
                # Recurse into non-dependency tables (e.g. `target`).
                yield from iter_dep_tables(value, full)

hits = []
for table_path, dep_key, dep_val in iter_dep_tables(data):
    # A dep value is either a plain version string (e.g. `lattice-api = "0.1"`,
    # no path/package) or an inline table that may carry `path`, `package`,
    # `git`, etc. The forbidden-NAME check must run for BOTH forms: when there
    # is no `package =` rename, the actual crate name IS the dep key, so a
    # string-form `lattice-api = "0.1"` must be flagged by dep_key. The
    # `package`/`path` reads only make sense for the inline-table form.
    actual_name = dep_key
    path_val = None
    if isinstance(dep_val, dict):
        actual_name = dep_val.get("package", dep_key)
        path_val = dep_val.get("path")
    if isinstance(actual_name, str) and (
        actual_name.startswith("lattice-") or actual_name.startswith("lancet-")
    ):
        hits.append(
            f"{manifest}: [{table_path}] {dep_key}: "
            f"forbidden product crate name '{actual_name}'"
        )
    if isinstance(path_val, str):
        resolved = os.path.normpath(os.path.join(manifest_dir, path_val))
        if under_crates_dir(resolved):
            hits.append(
                f"{manifest}: [{table_path}] {dep_key}: "
                f"product path dependency -> {resolved}"
            )
print("\n".join(hits))
PY
)

print_group "manifest forbidden deps (tomllib)" "$manifest_forbidden_deps"

# ── Manifest: workspace = true ───────────────────────────────────────────────

workspace_true_hits=$(
  grep -nE 'workspace\s*=\s*true' "$manifest" 2>/dev/null || true
)

print_group "manifest workspace inheritance" "$workspace_true_hits"

# ── Cargo metadata dependency graph ───────────────────────────────────────────

if ! metadata=$(cargo metadata --format-version 1 2>/dev/null); then
  print_group "cargo metadata" "failed to read workspace metadata"
  failures=$((failures + 1))
else
  # Walk the FULL transitive dependency closure of lattice-system-harness via
  # the cargo `resolve` graph and flag ANY dependency that is a workspace
  # member. This is name-agnostic: it catches neutral-named product crates
  # (e.g. net-guard) and indirect deps regardless of how the manifest declares
  # them (single/double quotes, `package =` renames, transitive pull-in). The
  # manifest tomllib check is the direct-dep front line with precise dep keys;
  # this is the resolved, transitive backstop. `resolve.nodes[].dependencies[]`
  # are real package ids, so the walk visits every transitive dependency.
  meta_hits=$(
    printf '%s' "$metadata" | python3 -c '
import json, sys
data = json.load(sys.stdin)
id2name = {p["id"]: p["name"] for p in data["packages"]}
workspace_members = set(data.get("workspace_members", []))
nodes = {n["id"]: n for n in data.get("resolve", {}).get("nodes", [])}
try:
    root_id = next(p["id"] for p in data["packages"] if p["name"] == "lattice-system-harness")
except StopIteration:
    print("metadata: lattice-system-harness package missing from workspace")
    raise SystemExit(0)
hits = []
seen = set()
stack = [root_id]
while stack:
    pid = stack.pop()
    if pid in seen:
        continue
    seen.add(pid)
    # Skip the harness package itself; only flag its dependencies. A dep is a
    # forbidden product crate if its resolved package is a workspace member
    # (any crate under crates/), regardless of name.
    if pid != root_id and pid in workspace_members:
        name = id2name.get(pid, pid)
        hits.append(f"metadata: forbidden workspace-member dependency {name}")
    node = nodes.get(pid)
    if not node:
        continue
    for dep_id in node.get("dependencies", []):
        stack.append(dep_id)
print("\n".join(hits))
'
  )
  print_group "cargo metadata product dependencies (transitive)" "$meta_hits"
fi

if [ "$failures" -gt 0 ]; then
  echo
  echo "system harness boundary check failed: $failures group(s)"
  exit 1
fi

echo
echo "system harness boundary check passed"