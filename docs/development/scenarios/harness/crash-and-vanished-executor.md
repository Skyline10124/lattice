---
status: active
---

# 后台崩溃与执行者消失

| 项 | 内容 |
| --- | --- |
| 目标 | 后台在任一会话库提交点退出后，重开不经任何写入即可读到这一轮的结局；随后的 `send` 或 `rewind` 直接进行，旧 Turn 的节点保持原样 |
| 前提 | 显式崩溃 feature 与匹配的产品二进制；在每个会话库提交点设置屏障：接受、开始请求、尝试停止（可重试与不可重试）、登记调用、调用开始、调用返回（含取消结果）与关闭 Step；核验后台身份与命中记录；脚本为带多个调用的多步 Turn |
| 相关测试 | `direct_crash_barriers`：`redb_commit_windows_read_conclusions_without_writes_and_preserve_facts`、`r2_s2_multicall_reconnect_busy_sigkill_preserves_unclosed_step`、`memory_only_cancel_crash_is_process_exit_and_next_send_runs_once`、`dispatch_deadline_tail_is_atomic_and_survives_crash`；`direct_background`：`vanished_owner_is_reported_by_the_session_head` |

## 操作

1. 命中屏障后以 SIGKILL 中断后台，保留前端独立退出的证据。
2. 新客户端先查询，再发送新消息或执行 `rewind`。

## 断言

- 查询保持只读；重开后不经任何写入即可读到结局，旧任务的模型请求与工具分发次数不变。
- 新消息被接受一次并执行，provider 请求只因新任务增加。
- 每个提交点重开后，每个 Step、尝试与槽位停在崩溃前最后提交的阶段，读出的结论符合[阶段与结论的对照表](../../../architecture/session/README.md#预留progress-与结论)。
- 一个 Turn 中，除最后一个 Step 外都以 `Continue` 关闭；`TurnSummary` 与最后一个 Step 的结论一致；停在 `Started` 的槽位结果为 Indeterminate。
- 只在内存中的取消得到 Failed（进程退出）；已写入的取消原因得到 Cancelled。
- 发送新消息或退回之后，旧 Turn 的节点不变。
- 对照[执行者消失后的 Turn](../../../architecture/session/recovery.md#执行者消失后的-turn)，记录每个提交点的停点、状态摘要、树形 `log`、`session turn --json`、provider 与 Tool 的调用次数，以及文件效果。
- 收尾：同时检查崩溃前后的自有进程组与 stdout/stderr 的 EOF；必要的状态与原始失败进入证据之后，再清理。
