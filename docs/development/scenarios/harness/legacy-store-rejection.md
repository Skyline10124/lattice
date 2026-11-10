---
status: active
---

# 旧库拒绝

| 项 | 内容 |
| --- | --- |
| 目标 | 状态根中存在旧版 `sessions.git` 或 `projects/<key>/invocations.git` 时，会话库打开失败并指出路径 |
| 前提 | 在自有状态根分别放置两种旧库目录 |
| 相关测试 | `direct_tree_session`：`legacy_stores_are_refused_before_any_new_database_or_request` |

## 操作

1. 执行 `session send` 与 `session list`。

## 断言

- 错误列出旧库路径。
- 状态根中没有新建的 `lattice.redb`，旧库内容不变，provider 没有收到请求。
- 记录错误输出、退出状态与状态根中的文件列表。
