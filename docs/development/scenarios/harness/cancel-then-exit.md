---
status: active
---

# 取消接管后退出

| 项 | 内容 |
| --- | --- |
| 目标 | 后台接管的取消意图，在前端立即退出后仍被处理 |
| 前提 | 保存原身份；分别控制后台实际接管提交后的准备与执行窗口（工具执行中的取消通过 `LATTICE_TEST_TOOLS` 假工具 `block` 与 `ready_path` 同步）；区分未建立 Turn 的明确拒绝（`configure` 被拒，或 `configure` 成功后提交被拒）与结果未知 |
| 相关测试 | `direct_background`：`busy_cancel_and_stop_do_not_serialize_distinct_sessions`；`direct_failures`：`direct_failure_sigint_while_stalled_commits_cancelled`；`pty_tui_smoke`：`pty_second_ctrl_c_preserves_cancel_acknowledgement`；`direct_crash_barriers`：`persisted_cancel_after_mutation_survives_crash_without_replay`、`cancelled_slot_live_and_crash_share_ordered_closure` |

## 操作

1. 单次取消得到 true 后立即退出前端，放行后台直至独立收尾。
2. 明确拒绝且监督退场后，以同一 TurnId 重新提交。
3. 用 `session cancel <SESSION_ID>` 从另一个终端取消正在执行的一轮。

## 断言

- 按[取消规则](../../../architecture/session/recovery.md#取消与退出)核验实际操作的接管资格、同一载体的独立收尾，以及拒绝后的重新提交：`configure` 被拒时不建立 Turn（其中 `configure(New)` 被拒时不创建会话）；`configure` 成功而提交因配置被拒时会话已经存在，本次提交同样未建立 Turn。
- `session cancel` 取消的是该会话当时正在执行的一轮，没有时报告无可取消。
- 取消保留先前的终态槽位与假工具 `append_marker` 写入行，后续未执行槽位保持 `NotExecuted`，已开始调用在崩溃重开与 `rewind` 后不重放；取消回复 true 表示后台接管了取消，持久结局以 `session turn --json` 查询为准。
- 局部测试分别保护：两种回执次序下的一次补发、true 时不补发、五秒未知交接后提交连接仍在时只等回执、断开后才按 TurnId 查询，以及迟到的传输诊断；后台错误单独对照。
- 记录接管、未接管或未知的回执，前端退出，后续的 `session turn` 详情与标记文件行数；请求与诊断保留真实归属。
- 收尾：让后台完成必要收尾；结果未知时保留身份与诊断，原始失败保持原样。
