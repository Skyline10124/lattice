---
status: active
---

# 提交与找回

| 项 | 内容 |
| --- | --- |
| 目标 | 新建与续接会话、修改会话设置、核验配置与提交拒绝，中断后分别处理 `configure` 与 `submit` 的找回 |
| 前提 | 有效 profile 与凭据；`gpt-6-luna` 等在自定义 Chat Completions 带工具省去档位会报 400，需传 `--effort off` 或设 `effort = "off"` |
| 规则 | [模型引用](../../../architecture/llm/models.md)、[推理档位](../../../architecture/llm/reasoning.md)、[会话设置](../../../architecture/session/settings.md#configure-的计算基准与保存)、[档位规则](../../../architecture/session/settings.md#档位规则)、[配置拒绝](../../../architecture/session/acceptance.md#配置拒绝的原因)、[公开操作](../../../architecture/session/operations.md#实现模块与公开操作)、[身份与找回](../../../architecture/session/recovery.md#请求身份与找回)、[configure 的找回](../../../architecture/session/recovery.md#configure-的找回) |

## 命令

### 提交与修改设置

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

# 没有模型的草稿会话：首次提交被拒后，用同一 SessionId 选定模型继续提交
lattice --json run "$PROMPT" --agent <无模型的agent>
DRAFT_SESSION_ID="替换为该次首条 submit 记录中的 session_id"
lattice --json run "$PROMPT" --session "$DRAFT_SESSION_ID" --model anthropic/claude-sonnet-4-6
```

### 中断后找回

```sh
TURN_ID="替换为首条 submit 记录中的 turn_id"
lattice --json session turn "$TURN_ID"
```

## 结果

### 提交与修改设置

| 结果 | 含义与下一步 |
| --- | --- |
| `configure`：`configured` | 会话设置已保存，返回 `head` 与可选的 `adjustment`；接着发起 `submit` |
| `configure`：`rejected` | 设置校验、项目绑定或会话状态被拒，随后以同一错误输出 `submit` 的 `rejected` 回执，本轮不进入提交（`configure(New)` 被拒时不创建会话） |
| `submit`：`accepted` | 这一轮的接受事务已完成（会话设置在此之前已保存）；继续观察或按 `turn_id` 查询 |
| `submit`：`rejected`（`configure` 已成功或跳过） | 会话已经存在，本轮未建立；例如没有模型的草稿会话首次提交因 `bad_request` 被拒，选定模型后可对同一 `SessionId` 继续提交 |
| `rejected`（`busy`） | 同会话或全局槽位正忙；空闲后重发 |
| `rejected`（`bad_request` / `not_found`） | 参数、配置、档位或会话不存在被拒；按诊断修正后重发 |
| `rejected`（`conflict`） | 按原因码分流：`turn_id_used` 用 `session turn <TURN_ID>` 查询；`binding_mismatch` 表示会话属于另一个项目；`settings_changed` 表示会话设置已被修改，查看最新会话头核对后再重发；`session_exists` 表示 `configure(New)` 的 `SessionId` 已存在（CLI 自动重发遇到该码时读取会话头继续） |
| `configure`：`unknown`（紧跟 `submit`：`not_accepted`） | CLI 自动重发最多 2 次后配置结果仍不明，本次提交尚未发出；参数与配置文件不变时，以同一参数重发是安全的 |
| `submit`：`not_accepted` | 确定未被接受（发送前中断、前置 `configure` 结果为 `unknown`，或按 `TurnId` 查询为 `not_found`）；原工作区使用新 `TurnId` 重发 |
| `submit`：`unknown` | 提交发出后接受结果不明；按下表以首条 `submit` 记录中的 `turn_id` 查询找回 |
| TUI 启动时带 `--agent` | 在进入界面前的启动 `configure` 中立即生效并持久保存；之后的提交沿用当前会话设置 |

### 中断后找回

| 结果 | 含义与下一步 |
| --- | --- |
| `TurnSummary` 与 `steps` | `submit` 已接受；读结果或加 `--follow` |
| `{"status":"in_flight",...}` | `submit` 仍在准入（退出 `0`）；稍后再查或加 `--follow` |
| `error`（`not_found`） | `submit` 未被接受；原工作区重发（新 `TurnId`） |
