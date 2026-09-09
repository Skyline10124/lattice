---
status: active
---

# 领域文档指引

本页规定工程 skills 探索代码与修改领域文档时的读取顺序和术语写入位置。

## 布局

- `docs/architecture/`：领域架构合同。
- `docs/reference/glossary.md`：项目唯一领域术语表。
- `docs/adr/`：跨工作区的架构决策理由、取舍与历史。

## 读取顺序

完成根与目标路径适用的 `AGENTS.md` 要求后：

1. 从[文档索引](../README.md)定位并读取待修改领域的相关 active 合同，确认现行规范及明确保留的未承接来源。
2. 读取[领域术语表](../reference/glossary.md)，按词条链接核对关键区别和合同章节。
3. 需要理解决策理由、取舍或未承接条款时，再读对应 ADR；不以整套 ADR 代替现行合同。

必需输入缺失或不可读时，报告并暂停受影响部分。发现实现、术语、ADR 与合同冲突时，
明确冲突及承接关系，依治理规则交人类裁决，不自行选择竞争权威。

## 术语使用与回写

Issue 标题、重构提案、假设和测试名称中的领域概念使用术语表的名称，避免 `_Avoid_` 中的同义称呼。
缺少所需概念时，先确认是否属于项目领域，再由项目 `domain-modeling` skill 在授权范围内澄清。

获准修改术语时，唯一写者通过项目 `domain-modeling` 适配将词条写入 `docs/reference/glossary.md`。
保留简短定义、关键区别、合同链接及 `_Avoid_` 格式；完整规则仍由对应合同维护。
源工作流的术语定位、创建与回写均使用此路径，不重新创建根 `CONTEXT.md`。
具体源文件和格式读取要求见[项目 domain-modeling skill](../../.agents/skills/domain-modeling/SKILL.md)。
