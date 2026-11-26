---
status: active
---

# 后台与升级

| 项 | 内容 |
| --- | --- |
| 目标 | 安装并托管用户级后台服务、启停或重启后台、查询状态或跨版本停止更旧后台、手动运行后台，处理升级差异与脚本字段 |
| 前提 | `lattice` 与 `lattice-daemon` 安装在同一目录，IPC 协议版本与 `lattice.redb` 格式版本一致；跨协议版本升级可直接用新二进制重启或停旧后台 |
| 规则 | [宿主平台](../../../architecture/platform.md)、[ADR 0011](../../../adr/0011-im-gateway-inside-backend.md)、[停止后台](../../../architecture/session/recovery.md#停止后台)、[版本打开](../../../architecture/storage.md#身份版本与打开)、[测试工程](../../testing.md)、[用量费用](../../../architecture/llm/usage.md)、[关闭 Turn](../../../architecture/session/steps.md)、[模型候选](../../../architecture/llm/models.md#模型候选) |

## 命令

### 后台控制

```sh
# 写入（或更新）用户级 lattice.service 并启用与启动/重启后台（排空上限默认 60s）
lattice daemon install
lattice daemon install --drain-timeout 30s

# 通过薄封装或 systemctl 管理用户级服务与无人登录驻留
lattice daemon start
lattice daemon restart
systemctl --user status lattice
loginctl enable-linger

# 经本地 IPC 查询状态或排空停止后台（支持跨版本停止更旧后台；人类可读中文输出或 --json）
lattice daemon status
lattice --json daemon status
lattice daemon stop
lattice --json daemon stop

# 无 systemd user session 或隔离目录下手动运行后台
lattice-daemon --state-root /tmp/lattice-isolated-state --drain-timeout 30s
```

### 升级与输出字段适配

```sh
# 已由 systemd 托管时升级两个同目录二进制
cargo install --locked --path crates/lattice-cli
cargo install --locked --path crates/lattice-daemon
lattice daemon restart

# 同步变更 unit 参数（例如 --drain-timeout）或从手动/旧后台切换至 systemd 托管
lattice daemon stop
lattice daemon install
lattice --json daemon status
```

## 结果

### 后台控制

| 结果 | 含义与下一步 |
| --- | --- |
| `lattice daemon install` 退出 `0` | 已原子写入 `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lattice.service`（`TimeoutStopSec` 为排空上限加 30 秒），并依次完成 `systemctl --user daemon-reload`、按该文件绝对路径的 `systemctl --user enable` 与 `systemctl --user restart lattice.service`（已运行旧服务或更新排空时长时会优雅重启到最新配置；unit 不在运行中的 user manager 搜索路径里时，systemd 先将其链接进 manager 的配置目录），随后探测一次状态根，未发现协议不兼容的后台 |
| `lattice daemon install` 退出 `2` | 同目录缺失当前有效用户可执行的普通文件 `lattice-daemon`、传入了不支持的 `--json`、`--drain-timeout` 无效或加 30 秒后溢出，或可执行路径 / 状态根含无法写入 systemd unit 的字符；未写 unit 且未调用 `systemctl` |
| `lattice daemon install` 退出 `1`（写入 unit 失败） | 创建 `systemd/user` 目录、创建临时文件、写入、同步或重命名失败，unit 不一定写出，且未调用 `systemctl`；核对目录权限与磁盘空间后重试 |
| `lattice daemon install` 退出 `1`（`daemon-reload` 或 `enable` 失败） | unit 已写出，但 `systemctl --user daemon-reload` 或 `enable` 失败；不执行后续步骤，也不探测后台；核对用户级 systemd 会话与错误输出后重试 |
| `lattice daemon install` 退出 `1`（`restart` 失败） | unit 已写出并启用，探测一次状态根上的后台并按三种情况给出提示与对端 pid：已有后台应答时报出对端 pid 并提示先执行 `lattice daemon stop` 再执行 `lattice daemon start`；对端协议版本不兼容时保留版本诊断与对端 pid；无后台应答时提示执行 `systemctl --user status lattice` |
| `lattice daemon install` 退出 `1`（`restart` 成功但后台不兼容） | unit 已写出并启用，已成功执行 `restart`，但状态根上应答的后台协议不兼容：同目录文件较旧时安装与本前端同一批构建的 `lattice-daemon` 后执行 `lattice daemon restart`；运行的是别处文件时执行 `systemctl --user cat lattice` 核对生效的 unit 文件与 drop-in；后台较新时换用匹配的前端 |
| `lattice daemon start` / `restart` 退出 `0` / `1` / `2` | 不支持 `--json`（传入退出 `2`）；热路径单次调用 `systemctl --user start|restart lattice.service`，成功后探测一次状态根，未发现不兼容后台退出 `0`，不兼容退出 `1`（`start` 沿用普通不兼容诊断；`restart` 遇到更旧后台时指出对端 pid 与实际运行的可执行文件，并按它是否为本前端同目录的 `lattice-daemon` 提示重装 `lattice-daemon` 或执行 `lattice daemon install`）；`systemctl` 失败时查 `LoadState`，若为 `not-found` 提示先执行 `lattice daemon install`，否则单次探测状态根按占锁后台 / 不兼容后台 / 无应答给出诊断并退出 `1` |
| `unconfirmed_operations == 0` | 人类可读输出中文后台状态摘要与组件区块（无活跃或未挂载组件时为 `组件：无`），`--json` 输出 `epoch`、`stopping`、`owners`、`unconfirmed_operations` 与 `components`；`stop` 后状态根已释放（跨版本控制帧 `--json` 输出 `{"cross_version":true,"method":"control",…}`，历史旧后台信号兜底 `--json` 输出 `{"cross_version":true,"method":"signal","peer_pid":…}` 并在 stderr 注明无法读取排空计数） |
| `unconfirmed_operations != 0` | 有未确认收尾的操作（`daemon stop` 退出 `1` 并在标准错误提示）；查相关 Session |
| `stopping` / `not_running` | 锁未放开或断连（`daemon status` 复核），或无后台运行 |
| `lattice-daemon` | 手动运行后台守护进程（`--state-root` 必填且须为绝对路径，其下用 `lattice/`；`--drain-timeout` 默认 `60s`，参数错误退出 `2`，状态根被占用退出 `3` 并附对端进程号）；按 Ctrl-C（`SIGINT`）或发 `SIGTERM` 进入有界停机，排空完成退出 `0`，排空超时或排空中再次收到信号退出 `4`；故障注入限于隔离目录 |

### 升级与输出字段适配

| 结果 | 含义与下一步 |
| --- | --- |
| `incompatible_backend` | 本地 IPC 协议版本（当前为 `68`）与运行中后台不一致；若后台较旧，执行 `lattice daemon restart`（手动运行时直接执行 `lattice daemon stop` 或 `kill <pid>`）切换到新后台；若后台较新，换用匹配的新前端；`lattice daemon restart` 报重启后后台仍较旧时，按其提示更新 `lattice-daemon` 或执行 `lattice daemon install` |
| `transport`（零字节断开） | 更早后台或崩溃断开；核对存活与版本，必要时执行 `systemctl --user stop lattice` 或手动结束旧 `lattice-daemon` 后重启 |
| 拒绝格式版本较旧的库或 `*.git` 旧库 | 原库不变并列路径；由人存档或删除后，下一次请求直接生效，无需重启后台 |

脚本字段对照：

| 接口 | 旧字段 | 现行字段 |
| --- | --- | --- |
| 尝试 `usage` | `prompt_tokens`、`completion_tokens`、`total_tokens` | `input`、`cache_read`、`cache_write`、`output`、`reasoning`、`usage_kind` |
| 尝试费用 | `estimated_cost_usd` | `cost: { usd, exact } \| null` |
| Turn 汇总 | 三项合计与估算费用 | `attempts`、`partial_attempts`、五项 `{ known, complete, overflowed }`、`cost`、`kind` |
| `session usage` | `total`、`turns[].ordinal` | `turns[].start_node`（文本表首列 `NODE` 显示 `@<start_node>`）、`path_total`、`session_total` |
| `configure`、提交、`head`、Turn 起点与对话条目 | 无设置操作与档位字段；`UserMessage` 存接受时事实，`TurnView` 含 `user_node`；`ConversationEntry` 包裹物理 `Node` 与 `ordinal`/`turn_start` | `configure` 返回 `head` 与 `adjustment`；提交携带 `trigger_channel`、`inputs` 与可选 `expected_settings` / `expected_position`；`head` 含 `agent`、`provider`、`model`、`effort`、`settings_version` 及窗口 `has_older`，`history` 带 `has_more`；扁平对话条目为 `input` / `context_op` / `step`（不再包裹物理 `Node` 或 `turn_start`、`ordinal`，仅 `Step 0` 顶层带 `agent_name`），`TurnView` 含 `start_node` 与 `input_nodes` |
| `models`、`auth status` | 无档位、状态或后台诊断 | `models` 含 `input`、`supported_efforts`、`availability`（表增 `EFFORTS`、`INPUT`、`AUTH`）；`auth status` 与登录结果中的 `ProviderAuthStatus` 含可选 `diagnostic` |
| `daemon status`、`daemon stop` | 仅四项后台计数 | 新增必填 `components: ComponentStatus[]`（未启用组件不列入，空时为 `[]`） |
| `session turn` 槽位与工具错误 | `descriptor_identity`、`evidence`、分类错误对象 | `tool_identity`（无 `evidence`）；失败载荷为 `ToolError { code, message }` |
