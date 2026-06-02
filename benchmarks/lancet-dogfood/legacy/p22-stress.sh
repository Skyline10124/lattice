#!/usr/bin/env bash
# Targeted CEP stress checks: exec.command_run lifecycle, Guard L1, verify semantics,
# audit safety, and turn-level event mapping.
# Offline only — no API keys or network required.
set -euo pipefail

cd "$(dirname "$0")/../../.."

echo "== CEP stress: lancet-native exec.command_run =="
cargo test -p lancet-native --lib handlers::command -- --nocapture

echo "== CEP stress: lancet-native guard L1 =="
cargo test -p lancet-native --lib guard_l1:: -- --nocapture

echo "== CEP stress: lancet-native mutation + verify =="
cargo test -p lancet-native --lib handlers::mutation -- --nocapture

echo "== CEP stress: lancet-native shell executor =="
cargo test -p lancet-native --lib executor::shell::tests -- --nocapture

echo "== CEP stress: audit store + transport =="
cargo test -p lancet-audit --lib audit::tests -- --nocapture
cargo test -p lancet-dispatch -- --nocapture

echo "== CEP stress: lattice-api turn / CEP event mapping =="
cargo test -p lattice-api turn::tests:: -- --nocapture
cargo test -p lattice-api usage_run_turn -- --nocapture
cargo test -p lattice-api turn::live_tests -- --nocapture

echo "== CEP stress: agent live sink (no leaked server after cancel) =="
cargo test -p lattice-agent --test live_sink_and_cancel --features test-utils --

echo "CEP targeted stress: PASS"
