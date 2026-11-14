---
status: active
---

# 安装用户级后台服务

| 项 | 内容 |
| --- | --- |
| 目标 | `lattice daemon install` 在写 unit 前完成同目录有效用户可执行 `lattice-daemon`、排空时长边界与可执行路径/参数分位置 systemd 语法校验，原子写出 `lattice.service` 并按顺序调用 `systemctl --user daemon-reload` 与 `systemctl --user enable --now lattice`，`enable --now` 失败时经状态根探测给出三类诊断与对端 pid；`lattice daemon` 子命令组的帮助、人类可读输出与 JSON 模式错误均为脱敏中文 |
| 前提 | 把 `LATTICE_BIN` 与 `LATTICE_DAEMON_BIN` 复制到同一场景临时目录运行（不调用仓库构建目录之外的宿主路径，也不触碰真实 `~/.config/systemd/user`）；`PATH` 仅指向记录调用顺序与参数的假 `systemctl` 脚本，`HOME`、`XDG_CONFIG_HOME` 与 `XDG_STATE_HOME` 指向场景隔离目录（另测清除 `XDG_CONFIG_HOME` 回退到 `$HOME/.config`）。假 `systemctl` 只核验 CLI 的写入、调用顺序与失败诊断，不验证真实 `systemd --user` 解析 unit、启动 `lattice-daemon`、接收 `$NOTIFY_SOCKET` 就绪/停机通知或崩溃重启行为；真实 systemd 托管见手动卡片[后台与升级](../manual/daemon-and-upgrade.md)，停机排空与 `$NOTIFY_SOCKET` 通知见[停止后台与排空](stop-and-drain.md) |
| 相关测试 | `fast_daemon_install`：`fast_daemon_install_writes_unit_and_enables_service`、`fast_daemon_install_rejects_bad_daemon_binary_or_overflow_without_side_effects`、`fast_daemon_install_handles_daemon_reload_and_enable_failures_and_chinese_status_output`、`fast_daemon_install_distinguishes_exec_path_and_arg_escaping_and_rejects_invalid_paths`；`fast_cli_help`：`fast_lattice_daemon_group_chinese_help_and_argument_validation` |

## 操作

1. 在隔离临时目录中部署同目录的 `lattice` 与 `lattice-daemon`，分别在默认参数、`--drain-timeout 300ms` 以及清除 `XDG_CONFIG_HOME`（仅保留隔离 `HOME`）下执行 `lattice daemon install`，并连续执行第二次覆盖写入。
2. 分开构造两类特殊路径执行 `lattice daemon install`：一是可执行文件所在目录分别含 `$` 与 `%`、以及同时含空格、`$` 与 `%`（状态根保持普通路径）；二是可执行文件所在目录分别含单引号、双引号、反斜杠或制表符；三是可执行路径保持普通路径，而 `--state-root` 对应的 `XDG_STATE_HOME` 含空格、双引号、单引号、反斜杠、制表符、`%` 与 `$`，或含换行符。
3. 在同目录缺失 `lattice-daemon`、`lattice-daemon` 为目录、`lattice-daemon` 属主为当前用户但权限为 `0641`（其他用户位有执行位而当前有效用户无执行权限）、`--drain-timeout 18446744073709551600s`（加 30 秒后 `Duration` 溢出）及传入 `--json` 的条件下分别执行 `lattice daemon install`。
4. 由假 `systemctl` 注入 `daemon-reload` 失败，以及在无后台运行、兼容后台运行中和模拟旧协议监听者占用 `api.sock` 三种状态根情形下注入 `enable --now` 失败；模拟旧协议监听者的非阻塞 `accept`、帧读写与线程回收受同一截止时间约束。随后对运行中后台执行人类可读与 `--json` 的 `lattice daemon status`、`lattice daemon stop`。
5. 对 `lattice daemon`、`daemon status`、`daemon stop`、`daemon install` 执行 `--help` / `-h` 与非法参数检查，并在含伪密钥的参数上测试普通模式与 `--json` / `-j` 模式（含 `lattice --json daemon ...` 与 `lattice daemon install --json`）。

## 断言

- 成功安装退出 `0`，`${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lattice.service` 经同目录临时文件原子写出且无 `.lattice.service.tmp-*` 残留，内容与 [ADR 0011](../../../adr/0011-im-gateway-inside-backend.md) 第 3 节逐行一致：`--state-root` 为 `StateLayout::resolve()` 解析出的当前用户私有状态根父目录；默认排空时长生成 `--drain-timeout 60s` 与 `TimeoutStopSec=90s`，`--drain-timeout 300ms` 生成 `--drain-timeout 300ms` 与 `TimeoutStopSec=30300ms`；未设 `XDG_CONFIG_HOME` 时写到 `$HOME/.config/systemd/user/lattice.service`。
- `ExecStart` 严格区分可执行路径与后续参数并用手工期望 unit 文本逐字核对：可执行路径保留字面 `$`、`%` 写作 `%%`、含空白时整体加双引号，目录含单引号、双引号、反斜杠、制表符或控制字符时在写 unit 前以退出码 `2` 拒绝；后续参数（状态根、时长）按参数规则在需要时加双引号，转义反斜杠、双引号、制表符，并将 `%` 写作 `%%`、`$` 写作 `$$`，含换行或控制字符时以退出码 `2` 拒绝。
- 同目录缺失 `lattice-daemon`、目标不是普通文件、按 `accessat(..., EXEC_OK, EACCESS)` 核验当前有效用户不可执行（含 `0641`）、排空时长无效或加 30 秒溢出、路径不可表示以及传入 `--json` 时，均以退出码 `2` 结束，不创建或修改 `lattice.service`，也不调用 `systemctl`。
- 写好 unit 后严格按 `systemctl --user daemon-reload`、`systemctl --user enable --now lattice` 顺序调用：`daemon-reload` 失败时退出 `1`，保留已写出的 unit，不执行 `enable --now` 也不探测后台。
- `enable --now` 失败时退出 `1`，unit 已写出且只探测一次状态根后台，对端 pid 取自同一 Unix socket 连接的 `SO_PEERCRED`（旧协议分支同样在持有连接期间读取 `SO_PEERCRED`）：兼容后台应答时报出对端 pid 并提示先执行 `lattice daemon stop` 再执行 `systemctl --user start lattice`；旧协议对端应答时保留协议版本不兼容诊断与对端 pid；无后台应答时提示执行 `systemctl --user status lattice`。
- `lattice daemon` 组的 `--help` / `-h`、人类可读输出与错误均为中文；`install` 帮助写明不支持 `--json` 且不暴露 `--json` 选项；JSON 模式下的 `daemon` 组参数错误与 `install --json` 均以退出码 `2` 在标准输出给出 `{"type":"error","kind":"bad_request","message":...}`，中文 `message` 脱敏且不泄露伪密钥，`daemon status --json` 与 `daemon stop --json` 字段形状不变。
- 所有外部命令与模拟旧协议夹具均受 `ScenarioBudget` 与共享截止时间约束，未发生探测时在期限内失败并留下记录证据，假 `systemctl` 不替代真实 `systemd --user` 的启动、就绪与重启验证。
