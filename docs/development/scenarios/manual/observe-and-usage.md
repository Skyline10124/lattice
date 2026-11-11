---
status: active
---

# 查询、观察与用量

| 项 | 内容 |
| --- | --- |
| 目标 | 读取 Turn 详情、跟随运行轮、打开 TUI 视图，查询路径与全会话用量 |
| 前提 | 已有 `TURN_ID` 或 `SESSION_ID`（支持唯一前缀）；无需 profile 或凭据 |
| 规则 | [公开操作与用量](../../../architecture/session/operations.md)、[观察与重连](../../../architecture/session/recovery.md#观察与重连)、[TUI 重载](../../../architecture/tui/README.md#连接结局与重载)、[费用估算](../../../architecture/llm/usage.md#费用估算)、[statusline](../../../architecture/tui/layout.md#statusline) |

## 命令

### 查询与观察

```sh
lattice --json session turn "$TURN_ID"
lattice --json session turn "$TURN_ID" --follow
lattice tui "$SESSION_ID"
```

### 用量查询

```sh
lattice session usage "$SESSION_ID"
lattice --json session usage "$SESSION_ID"
lattice session usage "$SESSION_ID" --json
```

## 结果

### 查询与观察

| 结果 | 含义与下一步 |
| --- | --- |
| `session turn` | 返回 `TurnSummary` 与各 Step；成功固定退出 `0`，结局读 `TurnSummary` |
| `session turn --follow` | 输出 JSONL，结局见末步 `TurnSummary`，退出码按结局映射（`0` / `1` / `2`） |
| `lattice tui` | 打开视图；断连用 `/reconnect` 重建 |

### 用量查询

| 结果 | 含义与下一步 |
| --- | --- |
| 文本输出 | 逐轮列计数、尝试数与费用，末两行为“当前路径合计”与“全会话合计” |
| JSON 输出 | `{ session_id, position, turns, path_total, session_total }`（`session_total` 含原分支） |
