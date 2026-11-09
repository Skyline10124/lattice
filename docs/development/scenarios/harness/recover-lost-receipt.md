---
status: active
---

# 接受回执丢失后找回

| 项 | 内容 |
| --- | --- |
| 目标 | 回执丢失时按 TurnId 查询得到确定结果（已接受、在途或 NotFound），同一身份只被接受一次 |
| 前提 | 从第一条 `submit` 记录保存 TurnId 与 SessionId；设置公开 IPC 的可控未读回执窗口；涉及工具不重放核对的用例开启 `fake-tools` feature（配 `test-fake-tools` 二进制与 `LATTICE_TEST_TOOLS`） |
| 相关测试 | `direct_background`：`unread_acceptance_is_found_by_turn_id_and_resubmission_conflicts`、`daemon_restart_reads_closed_result_without_resubmitting`、`turn_and_session_identity_conflicts_are_rejected_without_dispatch`；`direct_crash_barriers`：`cli_recovers_accepted_receipt_after_backend_crash`、`admission_window_reports_in_flight_and_a_departed_submitter_is_not_admitted` |

## 操作

1. 在未读回执窗口断开提交连接，按 TurnId 查询。
2. 以同一身份重新提交一次。
3. 分别覆盖：向已有 Session 提交、不带 `--session` 的首次提交、后台持续运行与后台重启后的找回，以及原请求仍在准入时的查询。

## 断言

- 已接受的请求能按 TurnId 查到；原请求仍在准入时查询返回在途，准入结束后返回已接受或 NotFound。
- 以同一身份重新提交返回 Conflict，模型循环与工具执行次数（含假工具 `append_marker` 标记文件行数）不变。
- 不带 `--session` 的首次提交之后，只有一个 Session 与一个 Turn。
- 执行者消失的 Turn 停在已写入的进度，后续写操作直接进行。
- 回执丢失由未读回执窗口证明；provider 收到请求之后才杀前端，证明不了回执丢失。
- [请求身份与找回](../../../architecture/session/recovery.md#请求身份与找回)表中的其余各行由局部测试覆盖。
- 记录未读回执窗口的证据、查询结果、重新提交的回执、`session turn` 详情、`list` 输出与 provider 请求计数。
- 收尾：先让在途提交与回执驱动退场，再分别核验前后台清理与证据完整性。
