---
status: active
---

# 会话退回与分叉

| 项 | 内容 |
| --- | --- |
| 目标 | 查看完整会话树，退回到节点 `N` 或根节点 `0`（改写首条消息），并从该处分叉提交 |
| 前提 | 位于会话绑定的项目工作区内，且该会话无运行中的 Owner；工作区文件与历史库 `current` 不变 |
| 规则 | [表与 rewind](../../../architecture/session/README.md#表读取范围与-rewind)、[项目绑定](../../../architecture/session/concurrency.md#会话库与项目绑定) |

## 命令

```sh
lattice session list
lattice session log "$SESSION_ID"
NODE_ID="2"  # 替换为 log 中实际存在的节点序号
lattice session rewind "$SESSION_ID" "$NODE_ID"
lattice session send "从这里换一种思路继续。" --session "$SESSION_ID"

lattice session rewind "$SESSION_ID" 0
lattice session send "改写后的第一条消息。" --session "$SESSION_ID" --agent reader
```

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
| 输出 `current=<NODE_ID>` | 已移至目标节点，原分支与原 `TurnId` 查询保留；后续提交从此分叉 |
| `Busy` | 同 Session 正运行，位置不变；等待结束或先取消 |
| `BadRequest` / `NotFound` | 节点不存在、目录与绑定项目不符或会话不存在，位置不变 |
