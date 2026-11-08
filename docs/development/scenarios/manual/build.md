---
status: active
---

# 构建被测产物

| 项 | 内容 |
| --- | --- |
| 目标 | 安装 guest 目标与编码器，构建宿主 CLI 与 Source Tool Component，设置被测路径 |
| 前提 | Linux 宿主，获准安装或构建；Rust `1.96.0`（[`rust-toolchain.toml`](../../../../rust-toolchain.toml)）、`wasm32-unknown-unknown`、`wasm-tools 1.254.0` |
| 规则 | [宿主平台](../../../architecture/platform.md)、[测试工程](../../testing.md)、[工具开发](../../tool-development.md#开发来源与生成) |

## 命令

### 安装与宿主构建

指定 `-p lattice-cli --bin lattice`（`--workspace` 会按宿主链接 guest `cdylib`）：

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-tools --version 1.254.0 --locked

cargo build -p lattice-cli --bin lattice
export LATTICE_BIN="$(pwd)/target/debug/lattice"
export LATTICE_HARNESS_FAILURE_DIR="$(pwd)/target/harness-failures"
```

### Component 核验

```sh
cargo xtask tool-build --verify
```

## 结果

### 安装与宿主构建

| 结果 | 含义与下一步 |
| --- | --- |
| 工具链就绪 | `xtask ci` 核验三者（版本不符报漂移警告）；`rollout` 门禁 `rustc` 与 guest 目标 |
| `LATTICE_BIN` 就绪 | 传当前 worktree 产物的绝对路径；失败目录默认写 `crates/lattice-system-harness/target/harness-failures/` |

### Component 核验

| 结果 | 含义与下一步 |
| --- | --- |
| 退出 `0` 无警告 | 构建、编码、`validate`、`wasi:*` 导入面、WIT world 与摘要通过 |
| 退出 `0` 提示摘要漂移 | 功能与导入面通过，摘要漂移（warn-only） |
| 非零退出 | 构建、编码、`validate`、导入面或 WIT world 失败 |
