---
status: active
---

# Git 与 GitHub

本文规定 Git 与 GitHub 的操作流程，以及提交、Issue、PR 与评论的格式。身份与权限见 [Agent 身份与权限](agent-permissions.md)。

## 工具

| 用途 | 命令 |
| --- | --- |
| 只读检查 | `git --no-optional-locks --no-pager <status / diff / log / show>`；`diff` 加 `--no-ext-diff --no-textconv` |
| 以 bot 身份提交、推送 | `bot-run git commit ...`、`bot-run git push ...` |
| GitHub | `gh-app <gh 子命令>` |

`--no-optional-locks` 让只读检查保持只读，`git status` 由此不刷新索引。

## 流程

```mermaid
flowchart LR
  A[从最新的 origin/main 建分支] --> B[修改并验证]
  B --> C[按路径暂存，核对暂存区]
  C --> D[bot-run git commit]
  D --> E[bot-run git push -u origin 分支]
  E --> F[gh-app pr create]
  F --> G[维护者审查与合并]
```

- 分支名：`<type>/<主题>`，例如 `docs/governance-rewrite`；对应 Issue 时写 `<type>/<编号>-<主题>`。
- 暂存时按路径或变更块指定，暂存区只放本任务的改动。
- 操作结果不明（超时、断连）时，先用只读命令确认是否已经生效，再决定是否重试。
- 推送被拒时，在远程最新提交之上整理本地提交，再推送。

## 提交格式

```text
<type>(<scope>)<!>: <中文摘要>

<正文>

<尾注>
```

| 部分 | 规则 |
| --- | --- |
| `type` | `feat` 新功能 · `fix` 修复 · `docs` 文档 · `refactor` 重构 · `perf` 性能 · `test` 测试 · `build` 构建与依赖 · `ci` CI · `style` 格式 · `chore` 其他维护 · `revert` 撤销 |
| `scope` | 稳定的组件或领域名，例如 `store`、`tui`、`governance`；可省略 |
| `!` | 破坏性变化时加，正文写明影响与迁移方式 |
| 摘要 | 一行，写实际完成的变化 |
| 正文 | 可选；与标题空一行，写原因、影响与关键取舍 |
| 尾注 | `Refs: #编号` 关联任务；PR 完整解决某个 Issue 时写 `Closes: #编号`；有真实的共同作者时写 `Co-authored-by: 名称 <邮箱>` |

示例：

```text
fix(store): 打开会话库时拒绝缺少根节点的会话

异常退出可能留下没有根节点的会话记录。打开时按损坏报告，
后续写入从一份完整的会话库开始。

Refs: #123
```

## Issue 格式

标题用中文直陈问题、计划目标或实施对象。正文使用 [`.github/ISSUE_TEMPLATE/`](../../.github/ISSUE_TEMPLATE/) 中的模板，尖括号内的说明在发布前替换为实际内容，不需要的可选小节删除：

| 模板 | 用途 |
| --- | --- |
| [`bug.md`](../../.github/ISSUE_TEMPLATE/bug.md) | 登记错误行为、回归或缺失规则 |
| [`task.md`](../../.github/ISSUE_TEMPLATE/task.md) | 登记特性计划、重构提案（`待决定`）或已批准 ADR 的分阶段实施（`实施跟踪`） |

- Agent 创建时保留首行身份头 `> Agent：lattice-devbot[bot] · 类型：<缺陷 | 待决定 | 实施跟踪>`，并在 `## 目的` 写明来源与授权；非交互 CLI 创建时先读取对应模板文件，填好后经 `--body-file` 提交（`gh` 不支持同时传 `--template` 与 `--body-file`）。
- 已有合同或 ADR 写明的规则直接链接过去；核对代码写明基准提交与 `文件:行`，外部事实写明来源。
- 验收标准写正向、可核验的完成条件；留待他处处理的事项集中在 `## 范围之外`。
- 标签见 [分诊标签](../agents/triage-labels.md)；关闭 Issue 由维护者执行。

## PR 格式

标题与提交标题格式相同，概括整个 PR。正文按下面的模板写，尖括号内的说明在发布前替换为实际内容：

```markdown
## 目的

<要解决的问题与预期结果。关联：#编号，或说明来自哪次对话>

## 变更

<按行为或职责列出改动与关键取舍>

## 影响范围

<受影响的组件、接口、数据与使用者>

## 验证

<命令或检查方式 → 结果，注明适用的提交>
<未执行的检查及原因>

## 待决定

<需要维护者决定的问题、可选方案与建议；没有时写“无”>
```

- 正文与 base、head 之间的实际差异一致；追加提交后同步更新正文。
- 长日志放链接或附件，正文保留结论。
- 草稿用 GitHub 的 Draft 状态表达。

## 评论格式

```markdown
> Agent：lattice-devbot[bot] · 类型：<进展 | 审查 | 验证 | 待决定>

<结论>

<依据：文件:行、命令与结果、链接>
```

- 类型从四种中选一种：进展写完成部分、阻塞与下一步；审查定位到文件、行或提交；验证写检查对象、命令与结果；待决定写问题、方案与建议。
- 更正已发布的结论时，写明原结论、更正原因与新依据。
