---
status: active
---

# 构建被测产物

| 项 | 内容 |
| --- | --- |
| 目标 | 构建宿主 CLI，设置被测路径 |
| 前提 | Linux 宿主，获准构建；[`rust-toolchain.toml`](../../../../rust-toolchain.toml) 固定的 Rust 工具链 |
| 规则 | [宿主平台](../../../architecture/platform.md)、[测试工程](../../testing.md) |

## 命令

```sh
cargo build -p lattice-cli --bin lattice
export LATTICE_BIN="$(pwd)/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$(pwd)/target/harness-failures"
```

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
| 工具链就绪 | `xtask ci` 与 `rollout` 核验 [`rust-toolchain.toml`](../../../../rust-toolchain.toml) 固定的 `rustc` 版本 |
| `LATTICE_BIN` 就绪 | 传当前 worktree 产物的绝对路径；失败目录默认写 `crates/lattice-system-harness/target/harness-failures/` |
