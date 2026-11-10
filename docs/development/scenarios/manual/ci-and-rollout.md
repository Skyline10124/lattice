---
status: active
---

# 本地 CI、远程工作流与 rollout

| 项 | 内容 |
| --- | --- |
| 目标 | 运行文档检查、本地 CI 与 alpha rollout，对照门禁差异 |
| 前提 | 核对并发、内存与磁盘（保持缓存原状）；worktree 可用 `env -u LATTICE_BIN`；`rollout` 要求跟踪文件干净 |
| 规则 | [测试工程](../../testing.md)、[`ci.rs`](../../../../xtask/src/ci.rs)、[`ci.yml`](../../../../.github/workflows/ci.yml)、[`full-gate.yml`](../../../../.github/workflows/full-gate.yml)、[`rollout.rs`](../../../../xtask/src/rollout.rs) |

## 命令

### 本地与远程 CI

```sh
CARGO_BUILD_JOBS=4 cargo xtask docs-check
cargo xtask ci
env -u LATTICE_BIN cargo xtask ci
```

### Alpha rollout

```sh
cargo xtask rollout --channel alpha
cargo xtask rollout --channel alpha --out target/rollout/custom-alpha.json
```

## 结果

### 本地与远程 CI

| 结果 | 含义与下一步 |
| --- | --- |
| `docs-check` 通过 | 输出通过文件数；失败逐行报缺失项 |
| `xtask ci` 输出 `All checks passed.` | 两轮验证与带 feature 的 clippy 全部通过（第二轮用带 `test-fake-tools,test-crash-barriers` 的构建覆盖 `target/debug/lattice`）；单 crate 超 `LATTICE_TEST_TIMEOUT_SECS`（默认 `300` 秒，`CARGO_BUILD_JOBS=4`）时回收进程组终止 |

门禁差异（三处均执行两轮验证与测试 feature 的 clippy，见[门禁](../../validation-guide.md#门禁)）：

| 门禁 | 触发 | 独有检查 | 不包含 |
| --- | --- | --- | --- |
| `xtask ci` | 手动 | 工具链预检；逐 crate 超时；末尾 `tool-build --verify`（漂移仅警告） | rollout |
| `ci.yml` | `main`、PR | 纯 `.md` PR 只跑 `docs-check` | encoder、`tool-build --verify`、rollout |
| `full-gate.yml` | 每日、手动 | 装 encoder，跑无测试 feature 的 rollout，`always()` 跑 `tool-build --verify`（写 `component-verify.log`） | `docs-check` |

### Alpha rollout

| 结果 | 含义与下一步 |
| --- | --- |
| `verdict: "pass"` | 8 步（`clean_worktree` 至 `offline_correctness`）全 `passed`（测试步骤 `passed > 0`，release 产物不启用测试 feature） |
| `verdict: "fail"`（退出 `1`） | 某步 `failed`，后续 `blocked`；查记录中的 `detail` 与 `failure_artifacts` |
| `verdict: "fail"`（退出 `2`） | 工作树脏、Git 失败或参数错误；查 `rejection` |
