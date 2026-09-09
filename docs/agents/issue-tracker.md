---
status: active
---

# GitHub 任务跟踪

本仓库任务与 PRD 位于 `Skyline10124/lattice` 的 GitHub Issues。GitHub 操作通过当前宿主的命令执行工具调用 `gh-app`；Pi 使用 `hypa_shell`，Delta 使用其实际可用的命令工具。身份、读写授权及评论格式见 [Git 操作与协作记录](../governance/git-operations.md)。

## 操作入口

| 目的 | CLI 入口 |
| --- | --- |
| 读取任务 | `gh-app issue view <编号> --repo Skyline10124/lattice` |
| 列出任务 | `gh-app issue list --repo Skyline10124/lattice` |
| 创建任务 | `gh-app issue create --repo Skyline10124/lattice` |
| 发表评论 | `gh-app issue comment <编号> --repo Skyline10124/lattice` |
| 编辑任务或标签 | `gh-app issue edit <编号> --repo Skyline10124/lattice` |
| 关闭任务 | `gh-app issue close <编号> --repo Skyline10124/lattice` |

收集需求时读取正文、评论和标签；评论可用 `--comments`，结构化内容可用 `--json body,comments,labels`。写操作分别核对授权，创建使用当前仓库模板，关闭不以 Agent 自证代替人类验收。认证失败不回退个人凭据。

分诊标签见 [标签映射](triage-labels.md)。标签或 Agent 修改的正文/状态不构成人类授权。源流程的 triage 只处理 Issues；PR 使用对应 `gh-app pr` 操作，正式批准、合并与发布按治理边界执行。

## 历史编号

Codeberg 跟踪于 2026-07-24 迁移：旧 #1 复用 GitHub #23，旧 #2–#50 映射 GitHub #24–#72，即 `github = codeberg + 22`。

归档计划、设计、FINDINGS/FIX 历史标题及旧提交消息保留原上下文，遇 Codeberg 编号按此映射解释，不当作同号 GitHub 引用。根及 crate `AGENTS.md` 使用 GitHub 编号，不适用历史例外。

## Skill 用语

“发布到任务跟踪器”指在获准后写 GitHub Issue；未授权时返回草案。“获取相关任务”指读取映射后的 Issue、评论和标签，再核对真实授权、范围与验收要求。
