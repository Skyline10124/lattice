---
status: active
---

# 分诊标签

将共享 skills 的分诊角色映射到项目标签，不另建状态系统。操作入口见 [GitHub 任务跟踪](issue-tracker.md)，授权与生命周期见 [文档生命周期](../governance/document-lifecycle.md#快文档的生命周期)。

| 源 skill 标签 | 项目标签 | 含义 |
| --- | --- | --- |
| `needs-triage` | `needs-triage` | 等待维护者评估 |
| `needs-info` | `needs-info` | 等待报告者补充 |
| `ready-for-agent` | `ready-for-agent` | 规格已整理，作为 Agent 执行候选，仍须核验授权 |
| `ready-for-human` | `ready-for-human` | 需要人类实施 |
| `wontfix` | `wontfix` | 不安排处理 |

标签表达分类或状态，不代替明确人类指令；Agent 添加标签不能授权自身执行。修改标签须有对应写授权。远程实值与映射不符时报告差异，不自动创建标签或另建体系。
