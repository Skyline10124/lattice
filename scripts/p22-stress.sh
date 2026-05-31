#!/usr/bin/env bash
# Targeted CEP stress checks: command.run lifecycle, Guard L1, verify semantics,
# audit safety, and turn-level event mapping.
# Offline only — no API keys or network required.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "== CEP stress: lattice-cep-native command.run =="
cargo test -p lattice-cep-native --lib handlers::command -- --nocapture

echo "== CEP stress: lattice-cep-native guard L1 =="
cargo test -p lattice-cep-native --lib guard_l1:: -- --nocapture

echo "== CEP stress: lattice-cep-native mutation + verify =="
cargo test -p lattice-cep-native --lib handlers::mutation -- --nocapture

echo "== CEP stress: lattice-cep-native shell executor =="
cargo test -p lattice-cep-native --lib executor::shell::tests -- --nocapture

echo "== CEP stress: audit store + transport =="
cargo test -p lattice-cep-audit --lib audit::tests -- --nocapture
cargo test -p lattice-cep-dispatch -- --nocapture

echo "== CEP stress: lattice-api turn / CEP event mapping =="
cargo test -p lattice-api turn::tests:: -- --nocapture
cargo test -p lattice-api usage_run_turn -- --nocapture
cargo test -p lattice-api turn::live_tests -- --nocapture

echo "== CEP stress: agent live sink (no leaked server after cancel) =="
cargo test -p lattice-agent --test live_sink_and_cancel --features test-utils --

echo "CEP targeted stress: PASS"
