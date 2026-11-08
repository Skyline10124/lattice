---
status: active
---

# 接受后断开前端

| 项 | 内容 |
| --- | --- |
| 目标 | 后台接受的任务在 CLI 或 TUI 退出后继续执行 |
| 前提 | 在自有状态根用不带 `--session` 的 `session send` 创建 Session，保存请求身份；用 provider gate 控制接受后的执行进展 |
| 相关测试 | `direct_background`：`detached_cli_finishes_and_world_owns_daemon_cleanup`；`pty_tui_smoke`：`pty_detach_reloads_pending_and_result_without_cancelling` |

## 操作

1. 确认接受后结束前端进程组，再释放 gate。
2. 另起客户端，用原 TurnId 查询结果。

## 断言

- 后台完成原请求，结果与预期脚本相符。
- provider 与 Tool 的调用次数不因新客户端连接而增加。
- 断连只通过结束前端进程组构造，后台保持运行。
- 记录前端退出与输出 EOF、后台是否存活，以及最终的 `session turn` 详情。
