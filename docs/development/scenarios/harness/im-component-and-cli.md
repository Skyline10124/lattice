---
status: active
---

# IM 组件托管、自动重载与 CLI 凭据管理

| 项 | 内容 |
| --- | --- |
| 目标 | 守护进程启动时以 `0700` 准备 `~/.lattice/im` 并在默认无配置时将 IM 记为未启用（不出现在 `DaemonStatus.components`）；原子保存 `im/config.toml` 由 `inotify` 自动触发重新挂载；`lattice im auth set\|status\|remove` 与 `lattice im reload` 在文本与 `--json` 下保持退出码、挂载回执与旧实例保留回退一致；删除 `im/` 目录后经 `im reload` 重建 `0700` 目录并恢复 `inotify` 监视；全流程不联网且不回显任何秘密 |
| 前提 | 隔离 `HOME` 与 `0700` 私有状态根目录；以 `SSL_CERT_FILE=/nonexistent-cert` 与 `SSL_CERT_DIR=/nonexistent-dir` 显式启动真实 `lattice-daemon`，使 `TelegramHttpClient` 在本地立即返回可重试无根证书诊断而不发起任何外部 DNS 或 HTTPS 请求，同时已挂载网关正常保持存活 |
| 相关测试 | `fast_im_cli`：`fast_im_component_lifecycle_inotify_reload_auth_and_secret_redaction`；`fast_cli_help`：`fast_lattice_im_group_chinese_help_and_argument_validation` |

## 操作

1. 启动自有后台，检查 `~/.lattice/im` 权限，分别以文本与 `--json` 执行 `lattice daemon status` 与 `lattice im reload`。
2. 以 `0600` 临时文件原子替换写入合法 `~/.lattice/im/config.toml`，不执行 `reload`，轮询 `daemon status` 观察 `inotify` 自动挂载结果，再执行 `lattice im reload` 与 `lattice im auth status`。
3. 通过标准输入向 `lattice im auth set telegram` 传入测试凭据，核对 `~/.lattice/im/auth.json` 权限、`im auth status` 与 `daemon status`（记录后台 `epoch`）。
4. 将 `im/config.toml` 原子改写为损坏 TOML，等待 `inotify` 处理后查询 `daemon status`（核对后台 `epoch` 未变）并执行 `lattice im reload`（文本与 `--json`）；再将 `im/config.toml` 修复为合法内容并执行 `lattice im reload`。
5. 删除整个 `~/.lattice/im` 目录，核对后台未自动重建目录，随后执行 `lattice im reload`，检查重建后的 `~/.lattice/im` 权限，并再次写入 `im/config.toml` 验证无需手动 `reload` 即可由恢复的 `inotify` 自动触发重新挂载。
6. 重新录入凭据后连续两次执行 `lattice im auth remove telegram`（含 `--json`），并对非法平台名、命令行多余位置秘密参数、`help im auth ...` 及含控制字符或为空的凭据输入执行 `lattice im` 命令。

## 断言

- 初始状态下 `~/.lattice/im` 为 `0700` 真实目录，`--json daemon status` 的 `components` 为 `[]`，文本输出含 `组件：无`；`lattice im reload` 输出 `IM：未启用`（`{"status":"disabled"}`）并退出 `0`。
- 写入合法 `im/config.toml` 后，`inotify` 自动将 `components` 更新为缺凭据的 `unmounted`，此时 `im reload` 退出 `1`；`im auth set telegram` 写出 `0600` `im/auth.json` 并重新挂载为 `Mounted`（退出 `0`），`daemon status` 输出 `im：已挂载` 且后台 `epoch` 全程不变。
- 运行中改坏 `im/config.toml` 时保留原运行实例，`daemon status` 报告 `im：已挂载；仍按上一份配置运行；<原因>`（`configuration_error` 非空），`im reload` 返回 `using_previous_configuration` 并以退出码 `1` 结束；修复配置后 `im reload` 恢复 `Mounted` 并退出 `0`。
- 删除 `~/.lattice/im` 后不自动重建目录；执行 `im reload` 后重建 `0700` `~/.lattice/im` 并恢复 `inotify` 监视（返回未启用、退出 `0`），随后保存 `im/config.toml` 自动触发重新挂载。
- 首次 `im auth remove telegram` 输出 `telegram 凭据已删除`（`removed: true`），重复执行输出 `telegram 凭据未配置`（`removed: false`），均退出 `0`；`lattice im` 全部 `--help` 无英文残留，非法平台、多余命令行参数与非法凭据全部以退出码 `2` 拒绝，标准输出、标准错误与失败证据中零秘密回显。
