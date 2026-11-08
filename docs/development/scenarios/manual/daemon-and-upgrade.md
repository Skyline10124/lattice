---
status: active
---

# 后台与升级

| 项 | 内容 |
| --- | --- |
| 目标 | 查询、停止或前台运行后台，处理升级差异，适配脚本字段 |
| 前提 | 客户端与后台构建相符（IPC `LTLOCAL=`，`lattice.redb` 版本 `7`）；升级前备旧二进制 |
| 规则 | [停止后台](../../../architecture/session/recovery.md#停止后台)、[版本打开](../../../architecture/storage.md#身份版本与打开)、[重建边界](../../tool-development.md#开发状态诊断与重建边界)、[测试工程](../../testing.md)、[用量费用](../../../architecture/llm/usage.md)、[关闭 Turn](../../../architecture/session/steps.md)、[模型候选](../../../architecture/llm/models.md#模型候选) |

## 命令

### 后台控制

```sh
lattice --json daemon status
lattice --json daemon stop
lattice daemon serve --state-root /tmp/lattice-isolated-state
```

### 升级与输出字段适配

```sh
/path/to/old/lattice daemon stop
lattice --json daemon status
```

## 结果

### 后台控制

| 结果 | 含义与下一步 |
| --- | --- |
| `unconfirmed_operations == 0` | 输出 `epoch`、`stopping`、`owners` 与计数；`stop` 后可立即起新后台 |
| `unconfirmed_operations != 0` | 曾有未收尾操作；查相关 Session |
| `stopping` / `not_running` | 锁未放开或断连（`daemon status` 复核），或无后台运行 |
| `daemon serve` | 前台运行（`--state-root` 须绝对路径，其下用 `lattice/`，相对路径退出 `2`）；故障注入限于隔离目录 |

### 升级与输出字段适配

| 结果 | 含义与下一步 |
| --- | --- |
| `incompatible_backend` | 较旧后台（如 `LTLOCAL<`）用旧二进制 `stop`，较新换新二进制 |
| `transport`（零字节断开） | 更早后台或崩溃断开；核对存活与版本 |
| 拒绝格式 `6` 或 `*.git` 旧库 | 原库不变并列路径；存档或删除须获授权 |

脚本字段对照：

| 接口 | 旧字段 | 现行字段 |
| --- | --- | --- |
| 尝试 `usage` | `prompt_tokens`、`completion_tokens`、`total_tokens` | `input`、`cache_read`、`cache_write`、`output`、`reasoning`、`usage_kind` |
| 尝试费用 | `estimated_cost_usd` | `cost: { usd, exact } \| null` |
| Turn 汇总 | 三项合计与估算费用 | `attempts`、`partial_attempts`、五项 `{ known, complete, overflowed }`、`cost`、`kind` |
| `session usage` | `total` | `path_total`、`session_total` |
| 提交、`head`、`UserMessage` | 无覆盖字段 | 提交可选 `model`、`effort`；`head` 含当前与默认档位；`UserMessage` 存档位 |
| `models` | 无档位与状态 | 含 `supported_efforts`、`availability`；表增 `EFFORTS`、`AUTH` |
