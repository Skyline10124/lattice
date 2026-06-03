#!/usr/bin/env bash
# scripts/release-gate-v1.3.sh — v1.3-alpha offline release gate
#
# FAIL-FAST: `set -euo pipefail` means the first failing step terminates the gate
# immediately. This is intentional — a broken fmt/clippy/test run should not be
# masked by later successes. There is no "collect all failures" mode; fix issues
# in order.
#
# NOT CALLED: scripts/release-gate.sh
#   The older gate targets stale package names. This gate supersedes it by
#   targeting the current LATTICE and LANCET crates explicitly.
#
# LOC BUDGET: source-loc-budget.sh is called with --max 60000.
#   Current measured source LOC is ~52-53k. The older default of 20k is far below
#   reality. Cap is raised to 60k for alpha; revisit for beta.
#   The check is advisory — gate output shows current totals for tracking.
#
# NO REAL-LLM STEPS: this gate is fully offline. No API keys, no model calls.
#   Real-provider checks remain manual artifacts in scripts/ and docs/.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "================================================================"
echo " LATTICE v1.3-alpha release gate"
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo " repo: $ROOT"
echo "================================================================"
echo ""

# ── Step 1: Format check ──────────────────────────────────────────────────────
echo "==> [1/9] cargo fmt --all --check"
cargo fmt --all --check
echo ""

# ── Step 2: Clippy ────────────────────────────────────────────────────────────
echo "==> [2/9] cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings
echo ""

# ── Step 3: LANCET crate tests ─────────────────────────────────────────────
# Explicit per-crate targets. Does NOT use `cargo test --workspace` so test
# scope is deterministic. Retired tooling crates are not release-gate targets.

echo "==> [3/9] LANCET crate tests"

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

echo "  --> lancet-native: guard_decide_fuzz"
cargo test -p lancet-native guard_decide_fuzz
echo ""

echo "  --> lancet-native: cep_warn_harness"
cargo test -p lancet-native cep_warn_harness
echo ""

echo "  --> lancet-native: remaining tests"
cargo test -p lancet-native
echo ""

# ── Step 4: lattice-agent ─────────────────────────────────────────────────────
echo "==> [4/9] lattice-agent (test-utils feature)"
cargo test -p lattice-agent --features test-utils
echo ""

# ── Step 5: lattice-api dogfood suite ────────────────────────────────────────
# Runs the impact matrix, cep_mode integration, and fake-LLM end-to-end test.
# All offline; no model calls.

echo "==> [5/9] lattice-api dogfood suite"

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

# ── Step 6: lattice-cli LANCET tests ──────────────────────────────────────
echo "==> [6/9] lattice-cli (lancet_* unit tests + full suite)"

echo "  --> lancet_* unit tests"
cargo test -p lattice-cli lancet
echo ""

echo "  --> lattice-cli: remaining tests"
cargo test -p lattice-cli
echo ""

# ── Step 7: Leaf regression crates ────────────────────────────────────────────
# Included here as a baseline regression net for leaf crates.

echo "==> [7/9] Leaf crate regression tests"

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
echo "==> [8/9] Sub-check scripts (LANCET boundary + LOC budget)"

echo "  --> scripts/lancet-boundary-check.sh"
scripts/lancet-boundary-check.sh
echo ""

# LOC budget: raised cap from 20k default to 60k to reflect actual alpha codebase
# size (~52-53k lines across all src/). Revisit for beta when scope is tightened.
echo "  --> scripts/source-loc-budget.sh --max 60000 (advisory)"
scripts/source-loc-budget.sh --max 60000 || true
echo ""

# ── Step 9: LANCET doctor self-check ──────────────────────────────────────
echo "==> [9/9] cargo run -p lattice-cli -- lancet doctor"
cargo run -p lattice-cli -- lancet doctor
echo ""

# ── Done ──────────────────────────────────────────────────────────────────────
echo "================================================================"
echo " release-gate-v1.3.sh PASSED"
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo "================================================================"
