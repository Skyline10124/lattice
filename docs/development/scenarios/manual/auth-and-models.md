---
status: active
---

# 凭据与模型元数据

| 项 | 内容 |
| --- | --- |
| 目标 | 录入或移除凭据、登录与登出 ChatGPT，查看并刷新模型候选与元数据 |
| 前提 | 配置目录权限合规；真实 SIWC 需人工核验（设 `DISPLAY` / `WAYLAND_DISPLAY` 调 `xdg-open`，否则向 stderr 显示 URL） |
| 规则 | [凭据与登录](../../../architecture/llm/credentials.md)、[私有交换](../../../architecture/session/operations.md#凭据命令与私有交换)、[模型元数据](../../../architecture/llm/models.md) |

## 命令

### 凭据与登录

```sh
lattice auth set anthropic
lattice auth login chatgpt
lattice --json auth status
lattice auth logout chatgpt
lattice --json auth remove chatgpt
```

### 模型元数据

```sh
lattice models --provider anthropic
lattice models --auth
lattice --json models --provider anthropic
lattice models refresh
lattice --json models refresh
```

## 结果

### 凭据与登录

| 结果 | 含义与下一步 |
| --- | --- |
| `auth set` 成功 | 从终端或 stdin 写入 `auth.json` |
| `auth login chatgpt` | 目录失败（`catalog.status: "failed"`）仍登录成功（退出 `0`）；中断用 `auth status` 核验，登录后可用 `/model`、`models --provider chatgpt` 或 `chatgpt/<模型>` |
| `auth status` | 列各 provider 状态，及 `chatgpt` 邮箱与 `expires_at`（过期仍 `ready`） |
| `auth logout` / `remove` | 返回 `revocation`（`unconfirmed` 可到设置断开）；`remove` 另含 `removed` |

### 模型元数据

| 结果 | 含义与下一步 |
| --- | --- |
| `models [--provider <id>]` | 输出 `MODEL`、`CONTEXT`、`OUTPUT`、`EFFORTS`、`PRICE`、`AUTH`（无效标 `config-error` / `invalid`，原因写 stderr） |
| `models --auth` | 仅列 `ready` 候选，跳过未就绪警告 |
| `--json models` | 输出 `models` 与 `warnings` |
| `models refresh` | 全成功输出 `{"type":"models_refresh","status":"refreshed"}`；部分或全败退出 `1`（保留已更新项），带 `--provider` / `--auth` 退出 `2` |
