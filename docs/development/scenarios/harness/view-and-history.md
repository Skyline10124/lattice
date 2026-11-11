---
status: active
---

# 视图、慢观察者与历史查询

| 项 | 内容 |
| --- | --- |
| 目标 | 观察者最后持有的条目与关闭后用历史查询重读的一致；已保存的对话、生成中的文字、执行 outcome 与结局，各自的显示与失败边界互相独立 |
| 前提 | 多步 Turn 脚本（需要工具步骤的用例开启 `fake-tools` feature，配 `test-fake-tools` 二进制与 `LATTICE_TEST_TOOLS`）：可区分的用户与模型消息、工具结果；模型流中正文与 reasoning 交错到达（含 OpenAI 兼容流）；包含一次可重试的尝试失败 |
| 相关测试 | `direct_background`：`slow_watcher_cannot_block_execution_and_converges_to_history`、`concurrent_autostart_has_one_owner_and_midway_watch_matches_history`、`long_session_pages_by_turn_and_rewind_replaces_the_path`、`a_live_new_turn_does_not_block_old_closed_observation`、`daemon_restart_reads_closed_result_without_resubmitting`；`direct_crash_barriers`：`slow_window_and_closed_snapshot_deliver_exact_history`；`pty_tui_smoke`：`pty_tui_smoke_runs_turn_and_quits_cleanly`、`pty_mid_model_reconnect_shows_new_suffix_before_completion` |

## 操作

1. 在 Step 中途、模型段中途，以及模型已输出说明文字而工具仍在执行时，分别开始观察或重连。
2. 放慢一个观察者后继续读。
3. 用调度 gate 在关闭事务提交后、标记变化前开始推送。
4. Turn 关闭后 `rewind` 到较早的节点。
5. Turn 关闭后用新客户端以历史查询重读同一会话，并跨页取回更早的轮。

## 断言

- 各种观察情况下，观察者最后持有的条目与重读结果一致；同一节点收到的值只前进。
- 生成中的文字与 Step parts 合并相邻同类后一致；重试的尝试按尝试边界重新开始，重试提示在条目中，实时与重开时相同。
- 工具执行中重连，仍显示模型已输出的说明文字；TUI 假工具回合按实际输出字节长度显示工具块，并与 `session turn --json` 读出的持久 Step、槽位结论一致。
- 慢观察者只少收中间版本，内容完整；慢读或断连时后台照常执行。
- 最后一步关闭时，观察者已有带 `TurnSummary` 的条目。
- `rewind` 后会话头标明路径已替换，随后是新路径上的条目。
- 历史查询的页边界只落在 UserMessage 之前，轮次序号与从根数起的结果相同。
- 记录各观察者收到的会话头、条目与生成中的文字，连接诊断、工具身份、推送序列与 `session turn --json` 重读结果。
- 收尾：解除推送 gate，关闭观察者；保留未完整的输出与清理诊断。
