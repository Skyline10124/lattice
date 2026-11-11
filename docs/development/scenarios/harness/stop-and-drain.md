---
status: active
---

# 停止后台与排空

| 项 | 内容 |
| --- | --- |
| 目标 | stop 关闭新准入，等待已接管的工作与必要的持久收尾 |
| 前提 | 用 gate 保持已接管的操作在途，另备查询、取消与新提交的客户端；在回执之后的退场窗口装 gate（listener 关闭前、实例锁释放前的测试钩子） |
| 相关测试 | `direct_background`：`busy_cancel_and_stop_do_not_serialize_distinct_sessions` |

## 操作

1. 发起 stop，在等待期间查询或取消原请求，并尝试新提交。
2. 释放 gate。

## 断言

- 新工作被拒绝；原工作仍可查询与取消，stop 等待它收尾；未确认操作数按本进程的口径解释。
- 退场窗口被挡住时 stop 不返回；stop 返回时 `owner.lock` 已可取得、`api.sock` 已撤下。
- 实例锁始终不放开时，stop 在注入的短期限内以 Stopping 报超时并返回。
- 后台是否退出，以自有进程组退空为准。
- 记录准入关闭、已有请求的状态、stop 回执与自有后台进程组的退出。
