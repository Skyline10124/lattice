#!/usr/bin/env bash
# scripts/release-gate-v1.4.sh — v1.4-alpha release gate
#
# FAIL-FAST: `set -euo pipefail` means the first failing offline step
# terminates the gate immediately. This is intentional: a broken formatter,
# lint, package test, boundary check, dogfood lane, or doctor check should be
# fixed before later release evidence is trusted.
#
# OFFLINE DEFAULT: this gate does not require API keys, provider calls, public
# network, or live Linux primitives unless `--live` is supplied or
# LATTICE_LINUX_ISOLATION_LIVE=1 is already set.
#
# LIVE LANE: live physical proof is delegated to the existing Rust ignored
# Linux canary suite. The local gate runs the suite and prints its summary, but
# does not enforce release-record capable-host policy such as
# skipped_unsupported=0. That capable-host condition (failed=0 and
# skipped_unsupported=0) is owned by the CI/release-record layer, not by this
# local gate's exit status. The suite's own assertions still fail the gate on a
# genuine canary failure via cargo's exit code.
#
# LOC BUDGET: source-loc-budget.sh is called with --max 85000 as a visible
# advisory for the v1.4-alpha rebaseline. The check is non-blocking.
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/release-gate-v1.4.sh [--live]

LATTICE v1.4-alpha release gate.

Options:
  --live   Run the ignored Linux physical-boundary canary suite and print its
           summary. Capable-host policy (failed=0, skipped_unsupported=0) is
           judged by the release-record layer, not this gate's exit status.
  -h, --help
           Show this help.

Default mode is offline and deterministic: no API keys, provider calls, public
network, or live Linux primitives are required.
USAGE
}

live=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --live)
      live=true
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

if [ "${LATTICE_LINUX_ISOLATION_LIVE:-}" = "1" ]; then
  live=true
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "================================================================"
echo " LATTICE v1.4-alpha release gate"
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo " repo: $ROOT"
echo " live: $live"
echo "================================================================"
echo ""

# ── Step 1: Format check ──────────────────────────────────────────────────────
echo "==> [1/10] cargo fmt --all --check"
cargo fmt --all --check
echo ""

# ── Step 2: Clippy ────────────────────────────────────────────────────────────
echo "==> [2/10] cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings
echo ""

# ── Step 3: LANCET crate tests ────────────────────────────────────────────────
echo "==> [3/10] LANCET crate tests"

echo "  --> lancet-protocol"
cargo test -p lancet-protocol
echo ""

echo "  --> lancet-catalog"
cargo test -p lancet-catalog
echo ""

echo "  --> lancet-dispatch"
cargo test -p lancet-dispatch
echo ""

echo "  --> lancet-audit"
cargo test -p lancet-audit
echo ""

echo "  --> lancet-authority"
cargo test -p lancet-authority
echo ""

echo "  --> lancet-native: guard_decide_fuzz"
cargo test -p lancet-native guard_decide_fuzz
echo ""

echo "  --> lancet-native: cep_warn_harness"
cargo test -p lancet-native cep_warn_harness
echo ""

# HashLine retention is deterministic offline proof. Keep this out of the live
# physical proof lane so the gate does not relabel commit-baseline correctness
# as Linux kernel canary evidence.
echo "  --> offline correctness proof: HashLine commit baseline"
cargo test -p lancet-native commit_baseline
echo ""

# Cleanup stress, cleanup-blocks-commit metadata, and lower-drift denial are
# offline correctness proof. These focused fault-injection tests stay separate
# from live physical proof.
echo "  --> offline correctness proof: cleanup and artifact metadata"
cargo test -p lancet-native artifact_commit_blocks_cleanup_failures_before_lower_recheck
cargo test -p lancet-native artifact_overlay_commit_flow_cleanup_failure_blocks_commit
cargo test -p lancet-native cleanup_verify_discard_layer_reports_cleanup_failure_as_commit_blocking_metadata
cargo test -p lancet-native artifact_commit_lower_drift_blocks_commit_before_workspace_mutation
echo ""

echo "  --> lancet-native: remaining tests"
cargo test -p lancet-native
echo ""

# ── Step 4: lattice-agent ─────────────────────────────────────────────────────
echo "==> [4/10] lattice-agent (test-utils feature)"
cargo test -p lattice-agent --features test-utils
echo ""

# ── Step 5: lattice-api dogfood suite ─────────────────────────────────────────
echo "==> [5/10] lattice-api dogfood suite"

echo "  --> dogfood_impact (production-class matrix)"
cargo test -p lattice-api dogfood_impact
echo ""

echo "  --> cep_mode_integration"
cargo test -p lattice-api cep_mode_integration
echo ""

echo "  --> release_e2e_criterion_8 (fake-LLM e2e)"
cargo test -p lattice-api release_e2e_criterion_8
echo ""

echo "  --> lattice-api: remaining tests"
cargo test -p lattice-api
echo ""

# ── Step 6: lattice-cli LANCET and release-smoke tests ────────────────────────
echo "==> [6/10] lattice-cli (lancet_* + release smoke + full suite)"

echo "  --> lancet_* unit tests"
cargo test -p lattice-cli lancet
echo ""

echo "  --> release_smoke_release_gate"
cargo test -p lattice-cli release_smoke_release_gate
echo ""

echo "  --> lattice-cli: remaining tests"
cargo test -p lattice-cli
echo ""

# ── Step 7: Leaf regression crates ────────────────────────────────────────────
echo "==> [7/10] Leaf crate regression tests"

echo "  --> lattice-llm"
cargo test -p lattice-llm
echo ""

echo "  --> lattice-session"
cargo test -p lattice-session
echo ""

echo "  --> lattice-profile"
cargo test -p lattice-profile
echo ""

# ── Step 8: Sub-check scripts ─────────────────────────────────────────────────
echo "==> [8/10] Sub-check scripts (LANCET boundary + LOC advisory)"

# The boundary regression and LOC budget are offline correctness proof checks.
# LOC stays visibly advisory: its output is printed and non-blocking.
echo "  --> scripts/lancet-boundary-check.sh"
scripts/lancet-boundary-check.sh
echo ""

echo "  --> scripts/source-loc-budget.sh --max 85000 (advisory)"
scripts/source-loc-budget.sh --max 85000 || true
echo ""

# ── Step 9: LANCET doctor self-check ─────────────────────────────────────────
echo "==> [9/10] cargo run -p lattice-cli -- lancet doctor"
cargo run -p lattice-cli -- lancet doctor
echo ""

# ── Step 10: Optional live Linux physical-boundary canaries ───────────────────
if [ "$live" = true ]; then
  echo "==> [10/10] live Linux physical-boundary canaries"
  # Run the suite and print its summary. cargo's exit code still fails the gate
  # on a genuine canary failure (the suite asserts on Failed, not Skipped), but
  # skipped_unsupported is left diagnostic and judged by the release record,
  # not this gate.
  LATTICE_LINUX_ISOLATION_LIVE=1 cargo test -p lancet-native linux_isolation_live_canary_suite -- --ignored --nocapture
  echo ""
else
  echo "==> [10/10] live Linux physical-boundary canaries skipped (use --live)"
  echo ""
fi

# ── Done ──────────────────────────────────────────────────────────────────────
echo "================================================================"
echo " release-gate-v1.4.sh PASSED"
echo " release record: docs/v1.4-alpha-release-record.md is the capable-host evidence home; this local gate does not write or enforce that record requirement."
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo "================================================================"
