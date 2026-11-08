---
status: active
---

# 提交与找回

| 项 | 内容 |
| --- | --- |
| 目标 | 新建与续接会话、覆盖模型与档位、核验拒绝，中断后按 `TurnId` 找回 |
| 前提 | 有效 profile 与凭据；`gpt-6-luna` 等在自定义 Chat Completions 带工具省去档位会报 400，需传 `--effort off` 或设 `effort = "off"` |
| 规则 | [模型引用](../../../architecture/llm/models.md)、[推理档位](../../../architecture/llm/reasoning.md)、[profile 副本](../../../architecture/session/settings.md)、[配置拒绝](../../../architecture/session/acceptance.md#配置拒绝的原因)、[公开操作](../../../architecture/session/operations.md)、[身份与找回](../../../architecture/session/recovery.md#请求身份与找回) |

## 命令

### 提交与覆盖档位

```sh
PROMPT="概括这个项目的用途，并指出主要入口文件和目录。"
lattice --json session send "$PROMPT" --agent reader
lattice --json run "$PROMPT" --agent reader --model anthropic/claude-sonnet-4-6 --effort high

SESSION_ID="替换为首条 submit 记录中的 session_id"
lattice --json session send "继续检查测试入口。" --session "$SESSION_ID" \
  --model anthropic/claude-sonnet-4-6 --effort high
lattice --json run "继续。" --session "$SESSION_ID"
lattice --json run "重新读取 reader 配置。" --session "$SESSION_ID" --agent reader
lattice --json run "测试档位拒绝。" --session "$SESSION_ID" --effort max
```

### 中断后找回

```sh
TURN_ID="替换为首条 submit 记录中的 turn_id"
lattice --json session turn "$TURN_ID"
```

## 结果

### 提交与覆盖档位

| 结果 | 含义与下一步 |
| --- | --- |
| `accepted` | 已写副本；继续观察或按 `turn_id` 查 |
| `rejected`（`busy`） | 同 Session 正忙；空闲重发 |
| `rejected`（`bad_request` / `not_found` / `conflict`） | 配置、档位或会话被拒；`conflict` 改用 `session turn <TURN_ID>` 查 |
| `not_accepted` / `unknown` | 未发出、未接受或回执不明；中断按下节找回 |
| TUI 启动时带 `--agent` | 首个带它的提交被接受后生效，之前一直保留；之后的提交沿用会话副本 |

### 中断后找回

| 结果 | 含义与下一步 |
| --- | --- |
| `TurnSummary` 与 `steps` | 已接受；读结果或加 `--follow` |
| `{"status":"in_flight",...}` | 仍在准入（退出 `0`）；稍后再查或加 `--follow` |
| `error`（`not_found`） | 未接受；原工作区重发（新 `TurnId`） |
