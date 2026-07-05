#!/usr/bin/env bash
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BOLD='\033[1m'
RESET='\033[0m'

pass() { echo -e "${GREEN}✓${RESET} $1"; }
fail() { echo -e "${RED}✗${RESET} $1"; exit 1; }
step() { echo -e "\n${BOLD}── $1 ──${RESET}"; }

step "fmt"
cargo fmt --all --check && pass "fmt" || fail "fmt"

step "clippy"
cargo clippy --workspace --all-targets -- -D warnings && pass "clippy" || fail "clippy"

step "build lattice (debug)"
cargo build -p lattice-cli --bin lattice && pass "build lattice" || fail "build lattice"

step "system harness boundary"
scripts/system-harness-boundary-check.sh && pass "harness boundary" || fail "harness boundary"

step "test"
LATTICE_BIN="${LATTICE_BIN:-$(pwd)/target/debug/lattice}" \
LATTICE_HARNESS_FAILURE_DIR="${LATTICE_HARNESS_FAILURE_DIR:-$(pwd)/target/harness-failures}" \
  cargo test --workspace \
  && pass "test" || fail "test"

echo -e "\n${GREEN}${BOLD}All checks passed.${RESET}"
