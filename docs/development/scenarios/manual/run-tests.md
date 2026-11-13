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
cargo test -p lattice-session
cargo test -p lattice-server
cargo test -p lattice-agent --features test-utils

cargo test -p lattice-system-harness --test direct_tree_session
cargo test -p lattice-system-harness --test direct_local_turn
cargo test -p lattice-system-harness --test direct_background -- --list
cargo test -p lattice-system-harness --test direct_background
cargo test -p lattice-system-harness --test pty_tui_smoke

cargo test -p lattice-system-harness
cargo test --workspace
cargo test --workspace --release
```

### 显式 feature 目标

```sh
cargo build -p lattice-cli --bin lattice --features lattice-server/test-fake-tools
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --features fake-tools \
  --test direct_tree_session --test direct_failures --test pty_tui_smoke

cargo build -p lattice-cli --bin lattice --features lattice-server/test-fake-tools,lattice-server/test-crash-barriers
LATTICE_BIN="$(pwd)/target/debug/lattice" \
  cargo test -p lattice-system-harness --features fake-tools,crash-barriers \
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
| `--features fake-tools` | 场景在后台启动前通过 `LATTICE_TEST_TOOLS` 注册假工具并运行通过；已覆盖 `target/debug/lattice`，普通验证前重跑无 feature 的宿主构建 |
| `direct_crash_barriers` | 通过；已覆盖 `target/debug/lattice`，恢复前重跑无 feature 的宿主构建 |
