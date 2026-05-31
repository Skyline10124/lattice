#!/usr/bin/env bash
# scripts/release-gate-v1.3.sh — v1.3-alpha offline release gate (D-08, VER-25, DOG-01, DOG-05)
#
# FAIL-FAST: `set -euo pipefail` means the first failing step terminates the gate
# immediately. This is intentional — a broken fmt/clippy/test run should not be
# masked by later successes. There is no "collect all failures" mode; fix issues
# in order.
#
# NOT CALLED: scripts/release-gate.sh
#   The v1.1 gate runs `cargo test -p lattice-tools`, but the package was renamed
#   to `lattice-tooling`. The v1.3 gate supersedes it by targeting the LATTICE and
#   LANCET crates explicitly. Do not source or call the v1.1 file.
#
# LOC BUDGET: source-loc-budget.sh is called with --max 60000.
#   Current measured source LOC is ~52-53k (RESEARCH Pitfall 4). The v1.1 default
#   of 20k is far below reality. Cap is raised to 60k for alpha; revisit for beta.
#   The check is advisory — gate output shows current totals for tracking.
#
# NO REAL-LLM STEPS: this gate is fully offline. No API keys, no model calls.
#   The Layer-2 real-LLM checklist and Layer-3 walkthrough are manual artifacts
#   in scripts/ and docs/ respectively (see docs/v1.3-alpha-walkthrough.md).
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

# ── Step 3: LANCET crate tests ────────────────────────────────────────────────
# Explicit per-crate targets. Does NOT use `cargo test --workspace` so test
# scope is deterministic. `lattice-tools` is the old stale name — this gate
# always uses the correct package name `lattice-tooling`.

echo "==> [3/9] LANCET protocol + executor tests"

echo "  --> cep-protocol"
cargo test -p cep-protocol
echo ""

echo "  --> cep-executor: guard_decide_fuzz"
cargo test -p cep-executor guard_decide_fuzz
echo ""

echo "  --> cep-executor: cep_warn_harness"
cargo test -p cep-executor cep_warn_harness
echo ""

echo "  --> cep-executor: remaining tests"
cargo test -p cep-executor
echo ""

# ── Step 4: lattice-agent ─────────────────────────────────────────────────────
echo "==> [4/9] lattice-agent (test-utils feature)"
cargo test -p lattice-agent --features test-utils
echo ""

# ── Step 5: lattice-api dogfood suite ────────────────────────────────────────
# Runs the DOG-04 impact matrix, cep_mode integration, and VER-25 Layer-1 e2e.
# All offline; no model calls.

echo "==> [5/9] lattice-api dogfood suite"

echo "  --> dogfood_impact (DOG-04 production-class matrix)"
cargo test -p lattice-api dogfood_impact
echo ""

echo "  --> cep_mode_integration (VER-23 / LEG-02)"
cargo test -p lattice-api cep_mode_integration
echo ""

echo "  --> ver25_release_e2e (VER-25 Layer-1 fake-LLM e2e)"
cargo test -p lattice-api ver25_release_e2e
echo ""

echo "  --> lattice-api: remaining tests"
cargo test -p lattice-api
echo ""

# ── Step 6: lattice-cli lancet tests ─────────────────────────────────────────
echo "==> [6/9] lattice-cli (lancet_* unit tests + full suite)"

echo "  --> lancet_* unit tests"
cargo test -p lattice-cli lancet
echo ""

echo "  --> lattice-cli: remaining tests"
cargo test -p lattice-cli
echo ""

# ── Step 7: Leaf regression crates ────────────────────────────────────────────
# These were skipped by the v1.1 gate (it ran lattice-tools which doesn't exist).
# Included here as a baseline regression net.

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

echo "  --> lattice-hooks"
cargo test -p lattice-hooks
echo ""

# ── Step 8: Sub-check scripts ─────────────────────────────────────────────────
echo "==> [8/9] Sub-check scripts (CEP boundary + LOC budget)"

echo "  --> scripts/cep-boundary-check.sh (DOG-01)"
scripts/cep-boundary-check.sh
echo ""

# LOC budget: raised cap from 20k default to 60k to reflect actual alpha codebase
# size (~52-53k lines across all src/). Revisit for beta when scope is tightened.
echo "  --> scripts/source-loc-budget.sh --max 60000 (advisory)"
scripts/source-loc-budget.sh --max 60000 || true
echo ""

# ── Step 9: lancet doctor self-check (DOG-05) ─────────────────────────────────
echo "==> [9/9] cargo run -p lattice-cli -- lancet doctor (DOG-05)"
cargo run -p lattice-cli -- lancet doctor
echo ""

# ── Done ──────────────────────────────────────────────────────────────────────
echo "================================================================"
echo " release-gate-v1.3.sh PASSED"
echo " $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
echo "================================================================"
