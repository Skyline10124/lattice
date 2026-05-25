#!/usr/bin/env bash
set -euo pipefail

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo build --workspace --all-features"
cargo build --workspace --all-features

echo "==> cargo test -p lattice-llm"
cargo test -p lattice-llm

echo "==> cargo test -p lattice-session"
cargo test -p lattice-session

echo "==> cargo test -p lattice-tools"
cargo test -p lattice-tools

echo "==> cargo test -p lattice-profile"
cargo test -p lattice-profile

echo "==> cargo test -p lattice-hooks"
cargo test -p lattice-hooks

echo "==> cargo test -p lattice-agent --features test-utils"
cargo test -p lattice-agent --features test-utils

echo "==> cargo test -p lattice-api"
cargo test -p lattice-api

echo "==> cargo test -p lattice-cli"
cargo test -p lattice-cli

echo "==> scripts/cep-boundary-check.sh"
scripts/cep-boundary-check.sh

echo "==> release gate passed"
