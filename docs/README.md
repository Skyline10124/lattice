# 文档索引

本页按主题提供文档入口，不定义独立规则。文档分类、效力与迁移方式由治理合同规定。

## 项目入口

- [项目介绍与快速开始](../README.md)：安装、配置与运行。
- [Agent 工作指引](../AGENTS.md)：治理文档读取要求、通用工作边界与工程约束。

## 治理

- [项目宪章](governance/charter.md)：权威来源、授权关系与 Agent 地位。
- [文档生命周期](governance/document-lifecycle.md)：文档分类、语言与命名、批准、修订和迁移。
- [架构合同写作](governance/architecture-contracts.md)：补充领域模型、结构与接口、数据流、行为语义及审查要求。
- [Agent 身份与权限](governance/agent-permissions.md)：独立身份、任务授权、凭据与操作边界。
- [Git 操作与协作记录](governance/git-operations.md)：默认只读、写操作授权，以及 commit、PR 和评论格式。

## 架构与设计

- [宿主平台](architecture/platform.md)：Linux 宿主支持范围与 WASM guest 边界。
- [本地工作区与变更历史](architecture/workspace.md)：草案，覆盖观察、文件变更、私有历史、Turn checkpoint 与显式 Rewind。
- [LANCET 运行时](LANCET.md)：工具目录、调用生命周期、宿主权限、工作区与审计接口。
- [TUI 设计](specs/2026-08-31-issue182-tui-cassette-futurism-spec.md)：终端布局、交互与配色规格。
- [Cell 准入与证据模型](specs/2026-07-26-issue28-cell-admission-design.md)：包含保留的模型与已退役的设计，先阅读文件开头的更新说明。

## 开发与验证

- [测试与验证](TESTING.md)：测试分层、验证责任、执行命令与 CI 入口。

## 参考资料

- [领域术语](../CONTEXT.md)：项目领域概念与命名。
- [Cassette Futurism 调色板](cassette-futurism-palette.jsx)：TUI 配色的持久设计资产，不随任务关闭归档；具体用法见 TUI 设计。

## 决策记录

- [ADR 0001](adr/0001-workspace-vcs-replaces-guest-git-promotion.md)：Workspace VCS 替代 guest Git 提升的设计。
- [ADR 0002](adr/0002-turn-scoped-msb-workspace.md)：回合级 Microsandbox 工作区镜像的设计。
- [ADR 0003](adr/0003-lancet-operates-directly-on-local-workspace.md)：LANCET 直接操作本地工作区。
- [ADR 0004](adr/0004-build-closed-wasm-tool-catalog.md)：构建期封闭的 WASM 工具目录。
- [ADR 0005](adr/0005-durable-turn-invocation-and-private-git-stores.md)：持久化 Turn、Invocation 身份与私有 Git 存储。

ADR 的用途与规则承接方式见[文档生命周期](governance/document-lifecycle.md#分类与归属)，不由本页重新判定。

## 其他既有材料

- [任务方案与记录](plans/)：既有实施方案、裁决与验证记录，按各自授权及承接关系查阅。
- [Agent 辅助资料](agents/)：任务系统、标签映射与领域文档读取资料。
- [Effect 与 ToolHost 旧设计](specs/2026-07-01-issue27-effect-model-toolctx-design.md)：Issue #27 的非规范性设计材料，仅作追溯。

旧材料的效力核定与规则承接见[文档生命周期](governance/document-lifecycle.md#迁移与唯一有效入口)。

## 历史归档

- [归档索引](archive/README.md)：历史发布记录、已取代设计、旧计划与研究材料。
