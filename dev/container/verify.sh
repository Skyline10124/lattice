#!/usr/bin/env bash
set -u -o pipefail
cd /workspace || exit 1
exec 9>target/.container-verify.lock
flock -n 9 || { echo '已有容器验证任务正在执行' >&2; exit 1; }
run_dir=$(mktemp -d "$PWD/target/container-check.XXXXXXXX")
export LATTICE_BIN="$PWD/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$run_dir/harness-failures"
export CARGO_TERM_COLOR=never
printf '%s\n' "$run_dir" | tee target/container-check-latest
overall=0

step() {
    local name=$1
    shift
    printf '%s START %s\n' "$(date -u +%FT%TZ)" "$name" | tee -a "$run_dir/summary.log"
    /usr/bin/time -v -o "$run_dir/$name.time.txt" "$@" > "$run_dir/$name.log" 2>&1
    local result=$?
    printf '%s END %s exit=%s\n' "$(date -u +%FT%TZ)" "$name" "$result" | tee -a "$run_dir/summary.log"
    if [[ "$result" -ne 0 ]]; then
        overall=1
        tail -n 35 "$run_dir/$name.log"
    fi
    return "$result"
}

{
    uname -a
    id
    rustc -Vv
    cargo -V
    rustup target list --installed
    wasm-tools --version
    git --no-optional-locks rev-parse HEAD
    cat /etc/os-release
} > "$run_dir/environment.txt"
sha256sum Cargo.lock > "$run_dir/cargo-lock.sha256"
if ! step preflight python3 dev/container/preflight.py; then
    printf '1\n' > "$run_dir/exit-status"
    exit 1
fi
if step host-build cargo build --locked -p lattice-cli --bin lattice; then
    step workspace-tests cargo test --locked --workspace --features lattice-agent/test-utils --no-fail-fast
fi
if step crash-build cargo build --locked -p lattice-cli --bin lattice --features test-crash-barriers; then
    step crash-tests cargo test --locked -p lattice-system-harness --features crash-barriers --test direct_crash_barriers --no-fail-fast
fi
step restore-debug cargo build --locked -p lattice-cli --bin lattice
step harness-boundary cargo run --locked -p xtask -- harness-boundary
step lancet-boundary cargo run --locked -p xtask -- lancet-boundary
step tool-codegen cargo run --locked -p xtask -- tool-codegen --check
step wasm-build cargo run --locked -p xtask -- tool-build --verify
if step release-build cargo build --locked --release -p lattice-cli --bin lattice; then
    export LATTICE_BIN="$PWD/target/release/lattice"
    sha256sum "$LATTICE_BIN" > "$run_dir/release.sha256"
    step release-operator cargo test --locked -p lattice-system-harness --features release-operator --test release_operator release_operator_component_matrix -- --exact
    step release-smoke "$LATTICE_BIN" --help
fi
sha256sum --check "$run_dir/cargo-lock.sha256" > "$run_dir/lock-check.log" || overall=1
printf '%s COMPLETE exit=%s\n' "$(date -u +%FT%TZ)" "$overall" | tee -a "$run_dir/summary.log"
printf '%s\n' "$overall" > "$run_dir/exit-status"
exit "$overall"
