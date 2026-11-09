---
status: active
---

# 查看证据

| 项 | 内容 |
| --- | --- |
| 目标 | 查看 rollout 结论与步骤状态，定位本地与远程 CI 的失败 bundle、发布记录和自检日志 |
| 前提 | 已运行外置 harness、本地 CI 或 `rollout`（成功场景不写 bundle，先核对目录时间） |
| 规则 | [测试工程](../../testing.md) |

## 命令

```sh
jq '{commit_sha, verdict, rejection, steps: [.steps[] | {id, status}]}' target/rollout/alpha-latest.json
```

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
| `crates/lattice-system-harness/target/harness-failures/<scenario>/` | 单跑 harness 失败 bundle：依次查主断言、终止与输出完整性、清理或扫描错误 |
| `target/harness-failures/<scenario>/`、`target/rollout/harness-failures/<scenario>/` | 本地 CI 或显式目录失败 bundle（远程 `harness-failures-ubuntu-latest`、`harness-failures-full-gate`），及 rollout 失败 bundle（记入 `failure_artifacts`） |
| `target/rollout/rollout-alpha-<yyyymmddTHHMMSSZ>.json`、`alpha-latest.json` | 主记录（`--out` 可改）与最近记录（远程 `rollout-alpha-record`，`schema_version: 1`）；核对 `commit_sha`、`artifact`、`probes`、`steps`、`failure_artifacts`、`verdict`、`rejection` |
| `target/rollout/component-verify.log` | `full-gate` 的 `tool-build --verify` 输出；核对产物摘要漂移警告或校验错误 |
