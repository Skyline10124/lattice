# Agent 工作指引

本文是 Agent 的入口，说明先读什么、怎样推进任务、交付时报告什么。项目介绍见 [README.md](README.md)，文档导航见 [docs/README.md](docs/README.md)。

## 阅读顺序

| 时机 | 读取 |
| --- | --- |
| 每个任务开始时 | [项目宪章](docs/governance/charter.md)、[Agent 身份与权限](docs/governance/agent-permissions.md) |
| 设计、写代码或写测试前 | [工程规范](docs/development/engineering.md) |
| 修改某个组件前 | 目标路径沿途的 `AGENTS.md`，以及其中列出的合同 |
| 写或改文档前 | [文档写作](docs/governance/writing.md) |
| Git 或 GitHub 操作前 | [Git 与 GitHub](docs/governance/git-operations.md) |

## 推进任务

1. 读懂现有实现、调用关系和相关合同。
2. 简单任务直接实施；复杂任务先写一份短计划，写明范围、风险和验证方式。
3. 每次交付一个可审查、可验证的最小闭环，再按反馈继续。
4. 改动集中在当前任务及其必要关联上；提交只包含本任务的改动，工作区里其他人的改动保持原样。
5. 实现未通过检查时，修改实现。遇到以下情况，先说明情况，交维护者决定：
   - 扩大任务范围、修改公共契约或做破坏性迁移；
   - 需要为已有概念新增第二条路径或第二处状态，见[工程规范](docs/development/engineering.md#归属)；
   - 需要修改合同、验收标准或检查本身。

## 交付

交付报告包含三项：

1. 改了什么。
2. 验证了什么：命令、结果，以及未覆盖的范围。
3. 还有什么未解决。
