---
status: active
---

# 退回与分叉

| 项 | 内容 |
| --- | --- |
| 目标 | `rewind` 到树上任意节点后提交，会从该节点分叉，原分支保留；发给模型的上下文是根到该节点的路径 |
| 前提 | 一个多步且写过文件的 Turn，一个在工具执行后被取消的 Turn；两个 Turn 使用不同的 agent，后一个 Turn 的 agent 因此在 profile 副本中 |
| 相关测试 | `direct_tree_session`：`multi_step_two_agent_tree_rewind_preserves_files_and_old_turn`；`direct_crash_barriers`：`rewind_commit_survives_sigkill_and_keeps_both_branches`；`direct_background`：`long_session_pages_by_turn_and_rewind_replaces_the_path` |

## 操作

1. `rewind` 到前一 Turn 中部的 Step 节点，先读取对话再 `send`；分叉后用 `session turn` 查看原 Turn。
2. `rewind` 回原 Turn 的最后一个节点并读取对话。
3. 退回到前一 Turn 后不带 `--agent` 发送。
4. `rewind 0` 后不带 `--agent` 发送一次，再带 `--agent` 改写第一条消息。
5. 在一个 Turn 进行中时 `rewind`；`rewind` 到不存在的序号。

## 断言

- 原分支的节点仍在 `log` 中，新节点以目标节点为父。
- 每次读取的对话都是根到当时当前位置的路径：退回后、发送前止于目标节点；退回原 Turn 后恢复原对话；`rewind 0` 后为空。
- 请求的对话部分等于发送前读取的对话加上本次 UserMessage，经所选 provider 的规定投影；系统提示与工具定义单独核对。改写第一条消息的请求只含新消息，新消息的父节点为 `0`。
- `rewind` 不改变 profile 副本：未指定 agent 的发送（包括当前位置为 `0` 时）使用副本中的 agent，而不是目标节点所属 Turn 的 agent；带 `--agent` 的发送按该 agent 重新生成副本。
- 原 Turn 的 `session turn` 仍给出它自己的结局与 Step 节点；工作区文件不变。
- 进行中的 `rewind` 返回 Busy；不存在的序号被拒绝，当前位置不变。
- 记录树形 `log`、当前位置与会话头中的副本摘要、对话读取、`session turn` 输出、请求捕获与文件摘要。
