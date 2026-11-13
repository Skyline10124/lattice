---
status: active
---

# 停止后台与排空

| 项 | 内容 |
| --- | --- |
| 目标 | IPC `stop`、`SIGTERM` 与 `SIGINT` 共用有界停机顺序：关闭新准入，在 `--drain-timeout` 内排空已接管工作；超时或排空中再收信号时停写会话库并以退出码 `4` 结束 |
| 前提 | 用 `FakeLlmServer` 的 `ResponseGate` 保持已接管 `Turn` 在途，另备查询、取消与新提交客户端；经统一的后台生命周期接口按需传入 `--drain-timeout` 显式启动直接子进程后台；退场窗口的局部测试在回执之后持有 `InstanceLock` 或挂起尾段任务 |
| 相关测试 | `direct_background`：`busy_cancel_and_stop_do_not_serialize_distinct_sessions`、`sigterm_drains_inflight_turn_and_exits_zero`、`sigint_right_after_ready_exits_zero_and_releases_resources`、`sigterm_drain_timeout_exits_four_and_leaves_vanished_owner_turn`、`ipc_daemon_stop_drain_timeout_reports_unconfirmed_and_daemon_exits_four`、`second_sigterm_during_drain_triggers_immediate_timeout_exit_four`；`fast_cli_help`：`fast_daemon_serve_invalid_drain_timeout_exits_two` |

## 操作

1. 经公开 Local IPC `stop`、`SIGTERM` 或 `SIGINT`（含就绪后无在途 `Turn` 时发送 `SIGINT`）向自有后台发起停机；排空期间执行查询或取消原请求，尝试提交新工作，随后释放 `ResponseGate`，停机退场后显式启动新后台核对持久结局。
2. 以短 `--drain-timeout` 显式启动自有后台，在模型请求在途且阻塞时分别经 `SIGTERM` 与 `lattice daemon stop`（文本与 `--json`）触发排空超时，停机完成后显式启动新后台查询原 `Turn` 并续发新一轮；另对无效 `--drain-timeout` 运行 `daemon serve`。
3. 以长 `--drain-timeout` 显式启动自有后台，第一次发送 `SIGTERM` 进入排空并确认 `daemon status` 为 `stopping: true`，在远早于排空上限时发送第二个 `SIGTERM`。

## 断言

- 排空期间新工作准入返回 `Stopping`，原工作仍可查询与取消；`ResponseGate` 在期限内放行时原 Turn 正常提交 Completed，`daemon serve` 退出码为 `0`（就绪后立即收到 `SIGINT` 同样退出 `0`），`api.sock` 已撤下且 `owner.lock` 已可取得。
- 排空超时后，在途操作被计入 `unconfirmed_operations` 且会话库停写，`daemon serve` 退出码为 `4`；`lattice daemon stop` 以退出码 `1` 返回，标准错误包含未确认收尾与执行者消失的提示（`--json` 标准输出仍为 `DaemonStatus` JSON）。新后台启动后，原 Turn 以执行者消失的未关闭进度读出（`ProcessExit`），同一会话可继续发送新一轮。
- 排空期间收到第二个停机信号时立即按排空超时收尾，在显著早于原 `--drain-timeout` 的时间内以退出码 `4` 结束；无效 `--drain-timeout` 参数以退出码 `2` 拒绝。
- 退场窗口中 `InstanceLock` 尚未释放时客户端 `stop` 继续等待；返回时 `api.sock` 已撤下、`owner.lock` 已可取得；锁在注入期限内始终被持有时 `stop` 以 `Stopping` 返回。
- 后台是否完全退出以自有进程组退空为准；记录准入关闭、已有请求状态、`stop` 回执与自有后台进程组的退出码。
