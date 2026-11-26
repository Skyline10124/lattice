---
status: active
---

# 会话退回与分叉

| 项 | 内容 |
| --- | --- |
| 目标 | 查看完整会话树，区分分叉回退（`rewind`）与同路径上下文编辑（`context-op`），退回到节点 `N` 或根节点 `0`（改写首条消息）并从该处分叉提交 |
| 前提 | 位于会话绑定的项目工作区内，且该会话无运行中的 Owner；工作区文件不变 |
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

# 若只需保留当前分支历史、仅精简或替换特定图片/节点/工具结果，可改用 context-op 而不必 rewind
lattice session inspect "$SESSION_ID"
lattice session context-op "$SESSION_ID" --omit-image 1 <media>:<len>:<sha256>
lattice session trigger "$SESSION_ID"
```

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
| 输出 `current=<NODE_ID>` | 已移至目标节点，原分支与原 `TurnId` 查询保留；后续提交从此分叉 |
| `Busy` | 同 Session 正运行，位置不变；等待结束或先取消 |
| `Conflict`（`binding_mismatch`） | 当前工作区与会话绑定的项目不一致，位置不变 |
| `BadRequest` / `NotFound` | 目标节点不存在、工作区无效或会话不存在，位置不变 |
