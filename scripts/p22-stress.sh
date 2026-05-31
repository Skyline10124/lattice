#!/usr/bin/env bash
# Phase 22 targeted stress: command.run lifecycle, Guard L1, verify semantics, audit safety.
# Offline only — no API keys or network required.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "== P22 stress: cep-executor command.run =="
cargo test -p cep-executor --lib handlers::command -- --nocapture

echo "== P22 stress: cep-executor guard L1 =="
cargo test -p cep-executor --lib guard:: -- --nocapture

echo "== P22 stress: cep-executor mutation + verify =="
cargo test -p cep-executor --lib handlers::mutation -- --nocapture

echo "== P22 stress: cep-executor shell executor =="
cargo test -p cep-executor --lib executor::shell::tests -- --nocapture

echo "== P22 stress: CEP audit store + transport =="
cargo test -p cep-executor --lib audit::tests -- --nocapture
cargo test -p cep-executor --lib cep_audit_transport -- --nocapture

echo "== P22 stress: lattice-api turn / CEP event mapping =="
cargo test -p lattice-api turn::tests:: -- --nocapture
cargo test -p lattice-api usage_run_turn -- --nocapture
cargo test -p lattice-api turn::live_tests -- --nocapture

echo "== P22 stress: agent live sink (no leaked server after cancel) =="
cargo test -p lattice-agent --test live_sink_and_cancel --features test-utils --

echo "P22 targeted stress: PASS"
