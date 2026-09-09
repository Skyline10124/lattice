# 文档索引

本页按主题提供文档入口，不定义独立规则。文档分类、效力与迁移方式由治理合同规定。

## 项目入口

- [项目介绍与快速开始](../README.md)：安装、配置与运行。
- [Agent 工作指引](../AGENTS.md)：治理文档读取要求、通用工作边界与工程约束。

## 治理

- [项目宪章](governance/charter.md)：权威来源、授权关系与 Agent 地位。
- [文档生命周期](governance/document-lifecycle.md)：文档分类、语言与命名、批准、修订和迁移。
- [架构合同写作](governance/architecture-contracts.md)：领域模型、结构与接口、数据流、行为语义及审查要求。
- [Agent 身份与权限](governance/agent-permissions.md)：独立身份、任务授权、凭据与操作边界。
- [Git 操作与协作记录](governance/git-operations.md)：默认只读、写操作授权，以及 commit 和评论格式。
- [PR 格式与行文规范](governance/pull-requests.md)：PR 标题、结构化授权、变更边界、验证记录与行文要求。

## 架构

- [宿主平台](architecture/platform.md)：Linux 宿主支持范围与 WASM guest 边界。
- [私有存储与持久化](architecture/storage.md)：状态寻址、三库归属、Git 内核、创建与打开、通用锁序、引用发布及持久性边界。
- [会话与 Turn](architecture/session.md)：会话历史、对话模型、请求接受、持久阶段、结果关闭与重开协调。
- [本地工作区与变更历史](architecture/workspace.md)：观察、文件变更、私有历史、Turn checkpoint 与显式 Rewind。
- [LANCET 工具执行](architecture/lancet.md)：工具组成、Host 权限、调用执行、证据与操作员读取。
- [Cell 权限与证据](architecture/cell.md)：Cell 模型、能力准入、Host 授权、结果计量与版本化证据。
- [TUI 交互与视觉](architecture/tui.md)：产品接线、键位、布局、停靠区、工具显示与配色。

## 开发与验证

- [工具开发与操作](development/tool-development.md)：manifest/生成链、profile、语义编辑、audit/status/doctor。
- [测试工程](development/testing.md)：测试分层、场景隔离、被测产物、执行资源与失败证据要求。
- [验证操作指南](development/validation-guide.md)：安装、构建、局部验证、显式 feature、本地 CI、rollout 和证据查看。
- [InvocationStore 诊断指南](development/invocation-store-diagnostics.md)：diagnostic-only 资格化工具、输入与版本、manifest 生成参数及历史证据保留。

## 参考资料

- [领域术语](reference/glossary.md)：项目唯一术语表，保留英文名称、中文定义、易混淆称呼及合同章节入口。
- [Cassette Futurism 配色资产](reference/cassette-futurism-palette.md)：完整色表、色阶语义与配色示例；TUI 实际用色见其合同。
- [Agent 辅助资料](agents/)：任务系统、标签映射与领域文档读取资料。

## 决策与历史

- [ADR 0001](adr/0001-workspace-vcs-replaces-guest-git-promotion.md)：Workspace VCS 替代 guest Git 提升的历史设计，已取代。
- [ADR 0002](adr/0002-turn-scoped-msb-workspace.md)：回合级 Microsandbox 工作区镜像的历史设计，已取代。
- [ADR 0003](adr/0003-lancet-operates-directly-on-local-workspace.md)：LANCET 直接操作本地工作区的理由与取舍。
- [ADR 0004](adr/0004-build-closed-wasm-tool-catalog.md)：构建期封闭的 WASM 工具目录。
- [ADR 0005](adr/0005-durable-turn-invocation-and-private-git-stores.md)：持久化 Turn、Invocation 身份与私有 Git 存储。

ADR 的用途与规则承接方式见[文档生命周期](governance/document-lifecycle.md#分类与归属)。
