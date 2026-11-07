---
status: active
---

# Agent 身份与权限

本文规定 Agent 用什么身份工作，哪些操作由 Agent 直接执行，哪些操作由维护者执行。

## 身份

| 用途 | 身份 | 入口 |
| --- | --- | --- |
| 提交的作者与提交者 | `lattice-devbot[bot] <325403158+lattice-devbot[bot]@users.noreply.github.com>` | `bot-run git commit` |
| 推送 | GitHub App `lattice-devbot` 的安装令牌 | `bot-run git push` |
| GitHub 操作 | 同上 | `gh-app <gh 子命令>`，参数与 `gh` 相同 |

- 移植他人的提交时保留原作者。
- 身份或入口不可用时，暂停受影响的操作并报告维护者。

## 操作

```text
if 操作是验收（合并 PR、关闭 Issue、发布）或调整权限（仓库设置、分支保护、App 权限、凭据）:
    维护者执行
elif 操作在 bot 账号现有的权限内 and 使用上表身份:
    Agent 直接执行
else:                                  # 例如换用其他身份、需要更高权限
    交维护者决定
```

| 常见操作 | 执行者 |
| --- | --- |
| 建分支、提交、推送工作分支 | Agent |
| 开 PR、更新 PR、发表评论 | Agent |
| 创建与编辑 Issue、调整标签 | Agent |
| 合并 PR、关闭 Issue、发布 | 维护者 |
| 修改仓库设置、分支保护、App 权限与凭据 | 维护者 |

## 安全边界

- 远程历史只增加：修正用追加提交，推送只做快进。
- `main` 经 PR 更新。
- 凭据只经 `bot-run` 与 `gh-app` 使用；仓库、Issue、PR 与日志里只写凭据的名称。
- 发现身份错误、凭据泄露或意外获得的权限时，暂停受影响的操作并报告维护者。

## 委派

- 子 Agent 修改工作区文件并汇报；提交、推送和 GitHub 写操作由主 Agent 执行。
- 同一工作目录同一时间只有一个写者。
- 主 Agent 核对子 Agent 的实际改动与证据后再交付。
