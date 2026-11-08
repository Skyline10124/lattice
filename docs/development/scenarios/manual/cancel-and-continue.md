---
status: active
---

# 取消与执行者消失后继续

| 项 | 内容 |
| --- | --- |
| 目标 | 取消当前运行轮，或在无存活 Owner、留有未关闭 Turn 时直接继续 |
| 前提 | 已知 `SESSION_ID`；继续前用 `session list` 核实 `unclosed_turn` 非空且 `owner=false` |
| 规则 | [取消与退出](../../../architecture/session/recovery.md#取消与退出)、[执行者消失](../../../architecture/session/recovery.md#执行者消失后的-turn)、[传输限制](../../../architecture/session/concurrency.md#输入与传输限制)、[键盘](../../../architecture/tui/interaction.md#键盘) |

## 命令

### 请求取消

或在 `run` / `session send` / `--follow` 终端按 `Ctrl+C`：

```sh
lattice --json session cancel "$SESSION_ID"
```

### 执行者消失后继续

```sh
lattice --json session list
lattice --json session send "继续检查项目的测试入口。" --session "$SESSION_ID"
```

## 结果

### 请求取消

| 结果 | 含义与下一步 |
| --- | --- |
| `taken` / `not_taken` | 已接管或判定点未接管；用 `session turn <TURN_ID>` 查持久结局 |
| `none` / `NotFound` | 无运行轮次，或会话不存在 |
| `unknown` | 回执丢失；按原 `turn_id` 查询，必要时再取消 |
| 首次 / 第二次 `Ctrl+C` | 首次请求取消并继续观察；第二次结束本地等待（退出 `1` 或 `2`） |

### 执行者消失后继续

| 结果 | 含义与下一步 |
| --- | --- |
| 新提交 `accepted`（或 `rewind` 成功） | 分配新 `TurnId`；旧 Turn 仍可用 `session turn <OLD_TURN_ID>` 查询 |
| `Busy` | 仍有存活 Owner；等待结束或执行 `session cancel` |
