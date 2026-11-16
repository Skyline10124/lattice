---
status: active
---

# Telegram 私聊接入与运维

| 项 | 内容 |
| --- | --- |
| 目标 | 配置并启用内置 Telegram 私聊网关，管理平台凭据与热重载，验证流式草稿、停止按钮与内置斜杠命令 |
| 前提 | 本地后台 `lattice-daemon` 已运行，目标工作区内已准备好可用的 `agent` 与模型凭据；已从 `@BotFather` 取得机器人 Token 与本人 Telegram 数字 `user_id` |
| 规则 | [IM 网关](../../../architecture/im.md)、[ADR 0011](../../../adr/0011-im-gateway-inside-backend.md)、[验证操作指南](../../validation-guide.md#判读结果) |

## 命令

```sh
# 1. 写入私有配置 ~/.lattice/im/config.toml（目录 0700，文件 0600）
install -d -m 0700 ~/.lattice ~/.lattice/im
umask 077
cat > ~/.lattice/im/config.toml <<'EOF'
[telegram]
allowed_senders = [123456789]
workspace = "/abs/path/to/your/workspace"
agent = "reader"
EOF

# 2. 从无回显终端录入 Bot Token（自动保存到 0600 的 ~/.lattice/im/auth.json 并挂载 IM）
lattice im auth set telegram

# 3. 查看凭据状态与后台组件挂载状态
lattice im auth status
lattice daemon status
lattice --json daemon status

# 4. 手动重载配置（例如修改凭据权限或重建被删的 ~/.lattice/im 目录后）
lattice im reload
lattice --json im reload

# 5. 移除平台凭据并卸载 IM 组件
lattice im auth remove telegram
```

在已加入 `allowed_senders` 的账号私聊窗口中依次发送：

- `/help` 或 `/start`：查看内置命令列表；
- 任意纯文本问题：观察流式草稿更新、草稿下方的停止生成按钮与最终分段回复；
- `/status`、`/usage`：查看当前绑定会话的模型设置与累计 token/费用；
- `/cancel`：取消当前正在执行的轮次；
- `/new`：清除当前私聊绑定的会话，下一条普通消息开启新会话。

## 结果

| 结果 | 含义与下一步 |
| --- | --- |
| `lattice daemon status` 显示 `组件：无` | `~/.lattice/im/config.toml` 不存在或未定义 `[telegram]` 表；IM 处于未启用（`Disabled`）状态 |
| `im：已挂载`（`lattice im reload` 退出 `0`） | 配置与凭据核验通过，网关已在后台运行并开始长轮询；修改保存 `im/config.toml` 会经 `inotify` 自动重新挂载且不中断在途轮次 |
| `im：已挂载；仍按上一份配置运行；<原因>`（`lattice im reload` 退出 `1`） | 新修改的配置文件或凭据文件不合规，后台保留修改前仍在运行的网关实例继续服务；按提示修复文件后保存或执行 `lattice im reload` |
| `im：未挂载；<原因>`（`lattice im reload` 退出 `1`） | 缺少凭据、目录/文件权限过宽（须属于当前用户且无组/其他用户权限，后台不自动改权限）、工作区不存在或运行期遭遇不可恢复台账错误；按原因修复后执行 `lattice im reload` |
| 删除 `~/.lattice/im` 后自动重载未触发 | 目录被删会使原 `inotify` 监视失效且后台不自动重建目录；执行一次 `lattice im reload` 即可重建 `0700` `~/.lattice/im` 并恢复自动监视 |
| 私聊发送图片/文件收到 `只支持文本消息`，或群聊无响应 | 网关仅服务 `allowed_senders` 白名单内的私聊纯文本消息；非私聊或非白名单消息静默确认丢弃 |
