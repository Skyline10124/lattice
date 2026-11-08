---
status: active
---

# 运行测试

| 项 | 内容 |
| --- | --- |
| 目标 | 运行 crate 测试、默认 harness 场景、全工作区测试及显式 feature 目标 |
| 前提 | 跑 harness 前设绝对路径 `LATTICE_BIN`；测 release 先构建 `--release` 宿主二进制并更新 `LATTICE_BIN` |
| 规则 | [工程规范](../../engineering.md#验证)、[测试工程](../../testing.md)、[`Cargo.toml`](../../../../crates/lattice-system-harness/Cargo.toml) |

## 命令

### 常规与 harness 测试

```sh
cargo test -p lancet-authority
cargo test -p lattice-api
cargo test -p lattice-agent --features test-utils

cargo test -p lattice-system-harness --test direct_tree_session
cargo test -p lattice-system-harness --test direct_local_turn
cargo test -p lattice-system-harness --test direct_operator
cargo test -p lattice-system-harness --test direct_background -- --list
cargo test -p lattice-system-harness --test direct_background
cargo test -p lattice-system-harness --test pty_tui_smoke

cargo test -p lattice-system-harness
cargo test --workspace
cargo test --workspace --release
```

### 显式 feature 目标

```sh
cargo test -p lattice-system-harness --features release-operator \
  --test release_operator release_operator_component_matrix -- --exact

cargo build -p lattice-cli --bin lattice --features test-crash-barriers
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --features crash-barriers \
  --test direct_crash_barriers
```

## 结果

### 常规与 harness 测试

| 结果 | 含义与下一步 |
| --- | --- |
| `N passed`（`N > 0`） | 已运行并通过；`0 passed` 未命中，精确匹配加 `-- --exact` |
| `--workspace --release` | 仅切测试二进制 profile，不替换 `LATTICE_BIN` |

### 显式 feature 目标

| 结果 | 含义与下一步 |
| --- | --- |
| `release_operator_component_matrix` | 输出 `1 passed`（默认 `--workspace` 与 `xtask ci` 不含） |
| `direct_crash_barriers` | 通过；已覆盖 `target/debug/lattice`，恢复前重跑无 feature 的宿主构建 |
