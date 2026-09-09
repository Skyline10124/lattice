# 文档归档

本目录保存设计、实施过程和原始运行证据。原文的日期、状态、数值、命令、源码位置与裁决均属于记录当时的上下文，不是当前执行入口；归档不证明旧任务已完成，不授权重新执行清理、迁移、资格化或 Git 操作。现行规则从[文档索引](../README.md)进入。

## 设计与计划

- [设计材料](designs/)：旧设计及规格。Issue #27 的 Effect/ToolHost 文档仍是非规范性、待复审历史，不因归档恢复效力。Cell 保留模型由 [Cell 合同](../architecture/cell.md)承接，native 双路径、SQLite、Turn/audit provenance 与 attestation 的退役不撤销。TUI 布局、交互与配色由 [TUI 合同](../architecture/tui.md)承接。
- [历史计划](plans/)：Direct-Local 与 gix 收口、#27 组成、#31 变更、#32 网络、#45/#46 测试迁移、#70 取消、#139/#141/#142/#144 持久协议、#182 视觉与停靠区、#185/#186/#187/#192 测试修复。文件名和原文保留，仅修移动后的相对链接；原文中的旧路径字面量、代码草图和任务写入要求用于追溯，不是当前操作指令。
- [ADR](../adr/)仍保存决策理由与修订关系，不把整套 ADR 当作当前合同。

当前规则的承接位置：

| 历史主题 | 现行入口 |
| --- | --- |
| Direct-Local、可见性、变更、历史与 Rewind | [工作区合同](../architecture/workspace.md) |
| 目录、Cell 组成、网络、调用与操作员读取 | [LANCET 合同](../architecture/lancet.md)、[Cell 合同](../architecture/cell.md) |
| 对话、Turn、关闭、候选修订与恢复 | [会话合同](../architecture/session.md) |
| 私有库、锁、屏障、strict open 与位置能力 | [存储合同](../architecture/storage.md) |
| 布局、停靠区、输入、渲染与公开 API | [TUI 合同](../architecture/tui.md) |
| 分层、命令结果、fake provider、隔离与失败证据 | [测试工程合同](../development/testing.md) |
| 配置、语义编辑、audit/status/doctor | [工具开发指南](../development/tool-development.md) |
| 安装、构建、feature、rollout 与证据查看 | [验证操作指南](../development/validation-guide.md) |
| qualification 输入、参数与命令 | [InvocationStore 诊断指南](../development/invocation-store-diagnostics.md) |

所有者本次明确解除 TUI 固定五文件及 #116 前 render 目录冻结，未授权新增卡片；废止独立 ProjectStoreKey admission-control 队列义务，实际锁等待仍计入 FinalizeBudget；取消跨机可复现证据自动升级硬门的义务，摘要漂移仍为 warning，其他完整性失败不变。这些裁决由上述领域合同承载，不回写原计划的历史决策正文。

[#118 原计划](plans/2026-08-22-issue118-tui-shell-design.md)的壳 ≤1250 行、未来 render ≤800 行上限也已由所有者取消；原文保留当时的切片预算和调整理由，不作为当前约束。

## 原始记录与未决状态

- [FINDINGS 原记录](records/FINDINGS.md)和 [FIX 原记录](records/FIX.md)：完整保留的根账本。原文保持原样，不继续追加新任务状态。INFO 的两项已明确 moot 且没有独立剩余要求，本次删除，不复制导航壳。
- FINDINGS 的 `H-GH34-03` 与 `M-TOOL-01` 保留 reserved/compiled Descriptor identity、启用集一致性与 Bundle 安装风险；现行 [#35](https://github.com/Skyline10124/lattice/issues/35)及 [LANCET 待核验边界](../architecture/lancet.md#来源与承接范围)承接，不能把 runtime 已统一等同于这些缺口已修复。
- `L-GH139A-10`、`L-GH139A-12` 及后续 syscall/规模增长问题由 [#153](https://github.com/Skyline10124/lattice/issues/153)承接。#139-A 正确性批准、#139-B 解阻与 qualification diagnostic-only 不改变历史 FAIL、撤回、样本或数学模型。
- 其余 OPEN、PARTIAL、ACCEPTED RESIDUAL 与环境条目本轮没有逐个运行产品验证。路径竞争、目录被移出根、载荷聚合、symlink 列举、DNS 阻塞、测试计时/环境隔离、旧 operator 读取与取消、对话复制成本和 Wasmtime 执行选择等均须结合当前实现核验，不能仅凭旧符号已删除宣称技术风险解决。工作区残余核验位置见[工作区合同](../architecture/workspace.md#实现与验证待办)，存储创建与恢复缺口见[存储合同](../architecture/storage.md#验证入口与承接)。
- FIX 中后续记录可能修订较早的状态，但不抹除早期证据。关于旧 SQLite、MSB、KVM、attestation、全量清理和发布的原命令不恢复为当前义务。代码重构提案、旧测试替换顺序与当次行数预算不能自行升级为永久产品保证。

## 其他既有归档

- [发布记录](releases/)：当时的发布说明、报告与证据模板。
- [Uninode 研究](uninode/)：尚未实现的 Uninode/context-compiler 研究及其实施计划。

归档保存证据，治理与验收仍按[文档生命周期](../governance/document-lifecycle.md)处理。
