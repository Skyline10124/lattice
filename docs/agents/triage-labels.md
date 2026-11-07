---
status: active
---

# 分诊标签

将共享 skills 的分诊角色映射到项目标签。操作入口见 [GitHub 任务跟踪](issue-tracker.md)，谁执行哪些操作见 [Agent 身份与权限](../governance/agent-permissions.md#操作)。

| 源 skill 标签 | 项目标签 | 含义 |
| --- | --- | --- |
| `needs-triage` | `needs-triage` | 等待维护者评估 |
| `needs-info` | `needs-info` | 等待报告者补充 |
| `ready-for-agent` | `ready-for-agent` | 规格已整理，Agent 可以开始执行 |
| `ready-for-human` | `ready-for-human` | 需要人类实施 |
| `wontfix` | `wontfix` | 不安排处理 |

标签表达任务的分类与状态；任务的范围与优先级由维护者决定。远程标签与本表不一致时，报告差异，由维护者调整。
