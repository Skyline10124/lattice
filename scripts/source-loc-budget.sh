#!/usr/bin/env bash
set -eu

usage() {
  cat <<'USAGE'
Usage: scripts/source-loc-budget.sh [--max N]

Counts every Rust source line under crates/*/src/**/*.rs and fails when TOTAL
exceeds the configured budget. Inline tests, doc comments, helpers, and all
active Rust source under crate src directories are included.
USAGE
}

max_lines=20000

while [ "$#" -gt 0 ]; do
  case "$1" in
    --max)
      if [ "$#" -lt 2 ]; then
        echo "error: --max requires a numeric value" >&2
        exit 2
      fi
      max_lines="$2"
      shift 2
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

case "$max_lines" in
  ''|*[!0-9]*)
    echo "error: --max must be a non-negative integer" >&2
    exit 2
    ;;
esac

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
cd "$repo_root"

if [ ! -d crates ]; then
  echo "error: crates/ directory not found from $repo_root" >&2
  exit 2
fi

total_lines=0
printed_rows=0

printf 'Strict Rust LOC budget: crates/*/src/**/*.rs\n'
printf 'MAX %s\n' "$max_lines"
printf '%-36s %10s %8s\n' "CRATE" "FILES" "LINES"

while IFS= read -r crate_dir; do
  [ -d "$crate_dir/src" ] || continue

  crate_files=0
  crate_lines=0
  while IFS= read -r -d '' file_path; do
    line_count=$(wc -l < "$file_path")
    line_count=${line_count//[[:space:]]/}
    crate_lines=$((crate_lines + line_count))
    crate_files=$((crate_files + 1))
  done < <(find "$crate_dir/src" -type f -name '*.rs' -print0 | sort -z)

  if [ "$crate_files" -gt 0 ]; then
    printf '%-36s %10s %8s\n' "$crate_dir" "$crate_files" "$crate_lines"
    total_lines=$((total_lines + crate_lines))
    printed_rows=$((printed_rows + 1))
  fi
done < <(find crates -mindepth 1 -maxdepth 1 -type d -print | sort)

printf '%-36s %10s %8s\n' "TOTAL" "$printed_rows" "$total_lines"

if [ "$total_lines" -gt "$max_lines" ]; then
  echo "FAIL: TOTAL $total_lines exceeds max $max_lines" >&2
  exit 1
fi

echo "PASS: TOTAL $total_lines is within max $max_lines"
