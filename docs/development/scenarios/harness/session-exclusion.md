---
status: active
---

# 同会话排他与跨会话并发

| 项 | 内容 |
| --- | --- |
| 目标 | 同 Session 忙时拒绝新请求；同项目不同 Session 的模型执行可以重叠 |
| 前提 | 同一项目中的两个 Session，用独立的 gate 识别各自的 provider 请求 |
| 相关测试 | `direct_background`：`busy_cancel_and_stop_do_not_serialize_distinct_sessions` |

## 操作

1. 保持第一个 Turn 在途，向同一 Session 发送另一请求。
2. 同时向第二个 Session 提交任务。

## 断言

- 同 Session 的新请求得到 Busy，未被接受，输入保留供稍后发送。
- 释放第一个 gate 之前，第二个 provider 请求已经到达；重叠以到达事件为证据，耗时只作参考。
- 取消其中一个 Session 正在执行的 Turn 不影响另一 Session 的在途 Turn。
- 收尾：释放所有 gate，分别保存每个请求的结果，再停止共用的自有后台。
