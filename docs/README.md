# 文档索引

本页按主题列出文档入口。文档的位置与写法见[文档写作](governance/writing.md)。

## 项目入口

- [项目介绍与快速开始](../README.md)：安装、配置与运行。
- [Agent 工作指引](../AGENTS.md)：阅读顺序、推进任务的方式与交付要求。

## 治理

- [项目宪章](governance/charter.md)：角色、规则怎样生效，以及 Agent 的工作原则。
- [Agent 身份与权限](governance/agent-permissions.md)：bot 身份，Agent 与维护者各自执行的操作，安全边界与委派。
- [Git 与 GitHub](governance/git-operations.md)：操作流程，以及提交、Issue、PR 与评论的格式模板。
- [文档写作](governance/writing.md)：文档位置、语言与状态、写法，以及架构合同与 ADR 的模板。

## 架构

- [宿主平台](architecture/platform.md)：Linux 宿主支持范围。
- [私有存储与持久化](architecture/storage.md)：全局 redb 会话库、后台单例、锁序与持久性边界。
- [会话与 Turn](architecture/session/README.md)：会话树、Step/Turn 与调用槽位、显式配置、会话设置与草稿会话、提交、会话视图与按轮分页、退回分叉及执行者消失后的结局。
- [模型接入](architecture/llm/README.md)：协议适配器、provider 配置与 features、模型引用与元数据、推理档位、用量与费用估算、凭据与 Sign in with ChatGPT。
- [TUI 交互与视觉](architecture/tui/README.md)：持久对话与临时文字、输入和后台接线、模型与推理档位的选择、重连与取消、工具显示、键位、布局与配色。

## 开发与验证

- [工程规范](development/engineering.md)：设计中的归属、代码可读性、测试判定、TDD 与验证范围。
- [测试工程](development/testing.md)：测试的原则：测试层、harness 的原则、场景的共同规则、失败证据与发布门禁。
- [验证操作指南](development/validation-guide.md)：动手验证的原则：产物、环境与后台、结果判读、证据与门禁。
- [场景卡片](development/scenarios/README.md)：每个自动场景与手动操作一张卡片，写明目标、前提与断言或命令与结果。

## 参考资料

- [领域术语](reference/glossary.md)：项目唯一术语表，保留英文名称、中文定义、易混淆称呼及合同章节入口。
- [Cassette Futurism 配色资产](reference/cassette-futurism-palette.md)：完整色表、色阶语义与配色示例；TUI 实际用色见其合同。
- [Agent 辅助资料](agents/)：任务系统、标签映射与领域文档读取资料。

## 决策与历史

- [ADR 0001](adr/0001-workspace-vcs-replaces-guest-git-promotion.md)：Workspace VCS 替代 guest Git 提升的历史设计，已取代。
- [ADR 0002](adr/0002-turn-scoped-msb-workspace.md)：回合级 Microsandbox 工作区镜像的历史设计，已取代。
- [ADR 0003](adr/0003-lancet-operates-directly-on-local-workspace.md)：LANCET 直接操作本地工作区的理由与取舍，已由 ADR 0009 取代。
- [ADR 0004](adr/0004-build-closed-wasm-tool-catalog.md)：构建期封闭的 WASM 工具目录，已由 ADR 0009 取代。
- [ADR 0005](adr/0005-durable-turn-invocation-and-private-git-stores.md)：持久化 Turn、Invocation 身份与私有 Git 存储；会话/调用存储与文件 Rewind 等部分由 ADR 0006 取代，Git 内核、工作区历史库与工作区锁等部分由 ADR 0009 取代，不透明回放材料的来源粒度由 ADR 0010 修订，项目键哈希绑定由 ADR 0012 改为规范项目路径。
- [ADR 0006](adr/0006-tree-session-and-global-redb-store.md)：tree-session、全局 redb 会话库、调用槽位与快照观察，取消文件 Rewind 和 Turn checkpoint；三层记录与自动关闭等部分由 ADR 0007 取代，观察与订阅、按 `--turn-id` 找回等部分由 ADR 0008 取代，工作区历史库、Cell 证据与操作员查询等部分由 ADR 0009 取代，当前 agent 的判定由 ADR 0010 取代，首次提交创建会话由 ADR 0012 取代。
- [ADR 0007](adr/0007-session-tree-as-single-authority.md)：会话树作为会话事实的唯一权威，Step 预留、由 `progress` 给出结论，取消自动关闭；读取时的交付方式由 ADR 0008 取代，槽位结论的 `evidence` 字段由 ADR 0009 移除，草稿会话允许删除由 ADR 0012 修订。
- [ADR 0008](adr/0008-backend-api-commands-view-queries.md)：本地后台 API 分为命令、视图与查询，按会话推送以节点为单位的对话条目，按轮分页，请求身份由客户端生成，`submit` 只接受新请求；`audit` 查询由 ADR 0009 移除，由客户端读取配置与凭据的规则由 ADR 0010 取代，`submit` 区分 `New`/`Existing` 由 ADR 0012 移入 `configure`。
- [ADR 0009](adr/0009-remove-legacy-lancet-tool-executor.md)：从本仓库整体剥离旧 LANCET、WASM 工具链、工作区历史库与审计/操作员接口，工具执行收窄为单一窄接口 `ToolExecutor`（进行中调用的 `deadline` 由执行器负责），身份与规范 JSON 原语归入 `lattice-conversation`；保留的 `ProjectStoreKey` 由 ADR 0012 改为规范项目路径。
- [ADR 0010](adr/0010-protocol-adapters-and-configured-providers.md)：模型接入按协议适配，provider 改为配置，厂商差异用 features 表达；模型写作 `<provider>/<model>`，元数据取自 models.dev，新增推理强度、用量细分与缓存、Responses、Interactions、Sign in with ChatGPT，配置与凭据由后台读取、key 保存在 `auth.json`，会话持有 profile 副本（其中 `[lancet]` 外部策略部分由 ADR 0009 移除），以及 TUI 的 `/model` 与 `/effort`；profile 副本与选择随提交生效的规则由 ADR 0012 取代。
- [ADR 0012](adr/0012-backend-owns-session-settings.md)：会话设置由后台持有并取代 profile 副本，`configure` 成为显式创建会话与修改设置的唯一命令，`submit` 只面向已有会话并支持核对 `expected_settings`；草稿会话逾 24 小时且无观察者时由后台清理，Session 项目绑定与 IPC 路径改存规范路径原始字节数组。
- [InvocationStore 历史诊断指南](development/invocation-store-diagnostics.md)：仅解释旧 Git ledger 的测量工具、参数与保留证据，不用于 redb 资格化或当前产品验收。

ADR 与合同的分工见[文档写作](governance/writing.md#文档放在哪里)。
