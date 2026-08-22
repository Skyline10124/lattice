#!/usr/bin/env bash
# Copyright 2026 Ast Rin
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"
ISOLATION="$ROOT/benchmarks/invocation-store/qualification_isolation.py"
CONTRACT="$ROOT/benchmarks/invocation-store/qualification-contract-v2.json"

# Run counts and the contract version live in the gate manifest, never here.
contract_field() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])' "$CONTRACT" "$1"
}
group_runs() {
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["groups"][sys.argv[2]]["runs"])' \
    "$CONTRACT" "$1"
}
CONTRACT_VERSION=$(contract_field qualification_contract_version)
HARNESS_SHAPE=$(contract_field required_harness_shape_version)

[[ $(findmnt -n -o FSTYPE --target "$ROOT") == btrfs ]] || {
  echo "qualification requires a Btrfs workspace" >&2
  exit 1
}
[[ -z $(git status --porcelain) ]] || {
  echo "qualification requires a clean worktree" >&2
  exit 1
}

QUALIFICATION_TMPDIR="$ROOT/target/invocation-store-qualification-tmp"
mkdir -p "$QUALIFICATION_TMPDIR"
QUALIFICATION_TMPDIR=$(realpath "$QUALIFICATION_TMPDIR")
[[ $(findmnt -n -o FSTYPE --target "$QUALIFICATION_TMPDIR") == btrfs ]] || {
  echo "qualification temp store requires Btrfs" >&2
  exit 1
}

SHA=$(git rev-parse HEAD)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
BUNDLE="$ROOT/target/invocation-store-bundles/${STAMP}-${SHA:0:12}"
mkdir -p "$BUNDLE/logs"
git bundle create "$BUNDLE/source-history.bundle" HEAD
git bundle verify "$BUNDLE/source-history.bundle" >/dev/null
SOURCE_HISTORY_SHA256=$(sha256sum "$BUNDLE/source-history.bundle" | cut -d' ' -f1)

# Compile once before any measured group. Measured runs exec this binary
# directly so cargo/rustc cannot compile between samples.
CARGO_JSON="$BUNDLE/logs/prebuild.json"
cargo test --release -p lancet-audit --test invocation_qualification --no-run --message-format=json \
  >"$CARGO_JSON"
QUAL_BIN=$(python3 "$ISOLATION" resolve-bin "$CARGO_JSON")

export TMPDIR="$QUALIFICATION_TMPDIR"
export LATTICE_INVOCATION_QUALIFICATION_TEMP_ROOT="$QUALIFICATION_TMPDIR"

CONDITIONS="$BUNDLE/run-conditions.jsonl"
export CONDITIONS
: >"$CONDITIONS"

python3 - "$BUNDLE/environment.json" "$SHA" "$SOURCE_HISTORY_SHA256" \
  "$CONTRACT_VERSION" "$HARNESS_SHAPE" <<'PY'
import json, os, pathlib, platform, shutil, subprocess, sys, time
out, sha, source_history_sha256, contract_version, harness_shape = sys.argv[1:]
tmpdir = os.environ["TMPDIR"]

def command(*args):
    return subprocess.check_output(args, text=True).strip()

def optional(*args):
    """A provenance field that may be unavailable; never fail the run for it."""
    try:
        return subprocess.check_output(args, text=True, stderr=subprocess.DEVNULL).strip()
    except Exception:
        return None

def read_sysfs(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8").strip()
    except OSError:
        return None

def os_release():
    fields = {}
    try:
        for line in pathlib.Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, _, value = line.partition("=")
                fields[key.strip()] = value.strip().strip('"')
    except OSError:
        return {"id": None, "version_id": None, "pretty_name": None}
    return {
        "id": fields.get("ID"),
        "version_id": fields.get("VERSION_ID") or fields.get("BUILD_ID"),
        "pretty_name": fields.get("PRETTY_NAME"),
    }

def store_device():
    source = optional("findmnt", "-n", "-o", "SOURCE", "--target", tmpdir)
    # Btrfs sources look like `/dev/nvme0n1p2[/@subvol]`.
    node = source.split("[", 1)[0] if source else None
    parent = None
    if node:
        parent = optional("lsblk", "-no", "PKNAME", node) or optional("lsblk", "-no", "KNAME", node)
        parent = parent.splitlines()[0].strip() if parent else None
    return {
        "source": source,
        "node": node,
        "parent_kname": parent,
        "model": read_sysfs(f"/sys/block/{parent}/device/model") if parent else None,
        "rotational": read_sysfs(f"/sys/block/{parent}/queue/rotational") if parent else None,
        "scheduler": read_sysfs(f"/sys/block/{parent}/queue/scheduler") if parent else None,
        "mount_options": optional("findmnt", "-n", "-o", "OPTIONS", "--target", tmpdir),
        "fs_options": optional("findmnt", "-n", "-o", "FS-OPTIONS", "--target", tmpdir),
    }

def cpu_governors():
    governors = sorted(
        {
            value
            for path in sorted(pathlib.Path("/sys/devices/system/cpu").glob("cpu*/cpufreq/scaling_governor"))
            if (value := read_sysfs(path)) is not None
        }
    )
    return governors or None

usage = shutil.disk_usage(tmpdir)
load1, load5, load15 = os.getloadavg()
environment = {
    # v2 adds distribution, store device/mount, scheduler, CPU governor, and
    # start-of-run free space / load average. v3 adds run-conditions.jsonl,
    # sampled before and after every run. Older bundles are never rewritten.
    # v4 records prebuilt-binary / retained-run-root isolation. It does not
    # add host exclusivity, PSI, or sample-deletion gates. v5 records the
    # qualification contract and harness shape the campaign was run under, so
    # a bundle cannot be adjudicated against a contract it never ran.
    "schema_version": 5,
    "qualification_contract_version": int(contract_version),
    "harness_shape_version": int(harness_shape),
    "run_conditions": "run-conditions.jsonl",
    "git_sha": sha,
    "source_history_bundle": "source-history.bundle",
    "source_history_sha256": source_history_sha256,
    "started_at_unix_ns": time.time_ns(),
    "platform": platform.platform(),
    "kernel": platform.release(),
    "distribution": os_release(),
    "filesystem": command("findmnt", "-n", "-o", "FSTYPE", "--target", os.getcwd()),
    "store_tmpdir": tmpdir,
    "store_filesystem": command("findmnt", "-n", "-o", "FSTYPE", "--target", tmpdir),
    "store_device": store_device(),
    "store_free_bytes_at_start": usage.free,
    "store_total_bytes": usage.total,
    "load_average_at_start": {"1m": load1, "5m": load5, "15m": load15},
    "cpu_governors": cpu_governors(),
    "build_profile": "release",
    "rustc": command("rustc", "--version"),
    "cargo": command("cargo", "--version"),
    "cpu_count": os.cpu_count(),
    "memory_bytes": int(os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")),
    "fixture_retention": "keep_until_summary",
    "qualification_test": "invocation_qualification",
}
with open(out, "w", encoding="utf-8") as handle:
    json.dump(environment, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

# Per-run host conditions, so an outlier can be attributed instead of argued
# about. Samples are never deleted; recorded conditions are what let one be
# annotated in place.
sample_conditions() {
  local group=$1 run=$2 phase=$3
  python3 - "$group" "$run" "$phase" <<'PYCOND'
import json, os, shutil, sys, time
from pathlib import Path

group, run, phase = sys.argv[1], int(sys.argv[2]), sys.argv[3]
load1, load5, load15 = os.getloadavg()
running = int(open("/proc/loadavg", encoding="utf-8").read().split()[3].split("/")[0])
record = {
    "group": group,
    "run": run,
    "phase": phase,
    "at_unix_ns": time.time_ns(),
    "load_average": {"1m": load1, "5m": load5, "15m": load15},
    "store_free_bytes": shutil.disk_usage(os.environ["TMPDIR"]).free,
    "running_processes": running,
}
temp_root = os.environ.get("LATTICE_INVOCATION_QUALIFICATION_TEMP_ROOT")
run_root = os.environ.get("LATTICE_INVOCATION_QUALIFICATION_RUN_ROOT")
if temp_root and run_root:
    record["fixture_retention"] = "keep_until_summary"
    record["run_root_rel"] = Path(run_root).resolve().relative_to(Path(temp_root).resolve()).as_posix()
with open(os.environ["CONDITIONS"], "a", encoding="utf-8") as handle:
    handle.write(json.dumps(record, separators=(",", ":"), sort_keys=True) + "\n")
PYCOND
}

run_group() {
  local test_name=$1 output=$2 group=$3 runs
  runs=$(group_runs "$group")
  : >"$output"
  for run in $(seq 1 "$runs"); do
    local run_root log
    run_root=$(python3 "$ISOLATION" allocate "$QUALIFICATION_TMPDIR" "$test_name" "$run")
    export LATTICE_INVOCATION_QUALIFICATION_RUN_ROOT="$run_root"
    log="$BUNDLE/logs/${test_name}-$(printf '%02d' "$run").log"
    sample_conditions "$test_name" "$run" before
    if ! "$QUAL_BIN" --ignored --nocapture --exact "$test_name" >"$log" 2>&1; then
      sample_conditions "$test_name" "$run" after
      echo "qualification test $test_name run $run failed; retaining $log and $QUALIFICATION_TMPDIR/runs" >&2
      return 1
    fi
    sample_conditions "$test_name" "$run" after
    local count
    count=$(grep -c '^QUALIFICATION_JSON=' "$log" || true)
    [[ $count == 1 ]] || {
      echo "expected one qualification record for $test_name run $run, got $count" >&2
      return 1
    }
    sed -n 's/^QUALIFICATION_JSON=//p' "$log" |
      python3 -c 'import json,sys; value=json.load(sys.stdin); value["run"]='"$run"'; print(json.dumps(value, separators=(",",":"), sort_keys=True))' \
      >>"$output"
  done
  unset LATTICE_INVOCATION_QUALIFICATION_RUN_ROOT
}

run_group local_ledger_stress "$BUNDLE/local-ledger.jsonl" local-ledger
run_group result_finalize_stress "$BUNDLE/result-finalize.jsonl" result-finalize
run_group cold_reopen_benchmark "$BUNDLE/cold-reopen.jsonl" cold-reopen

# Qualification gates are temporarily diagnostic-only. `benchmark` still fails
# closed on malformed or incomplete evidence and writes summary.json/summary.md,
# but a performance verdict does not block #139-B. Strict `qualify` remains for
# the follow-up redesign tracked in GitHub #153.
set +e
python3 "$ROOT/benchmarks/invocation-store/summarize_qualification.py" benchmark "$BUNDLE"
status=$?
set -e
if ! python3 "$ISOLATION" cleanup "$QUALIFICATION_TMPDIR"; then
  echo "warning: retained run-root cleanup failed under $QUALIFICATION_TMPDIR" >&2
fi

echo "$BUNDLE"
exit "$status"
