---
status: active
---

# 后台与升级

| 项 | 内容 |
| --- | --- |
| 目标 | 安装并托管用户级后台服务、查询或停止后台、手动运行后台，处理升级差异与脚本字段 |
| 前提 | `lattice` 与 `lattice-daemon` 安装在同一目录，IPC 协议版本与 `lattice.redb` 格式版本一致；跨协议版本升级前保留旧二进制 |
| 规则 | [宿主平台](../../../architecture/platform.md)、[ADR 0011](../../../adr/0011-im-gateway-inside-backend.md)、[停止后台](../../../architecture/session/recovery.md#停止后台)、[版本打开](../../../architecture/storage.md#身份版本与打开)、[测试工程](../../testing.md)、[用量费用](../../../architecture/llm/usage.md)、[关闭 Turn](../../../architecture/session/steps.md)、[模型候选](../../../architecture/llm/models.md#模型候选) |

## 命令

### 后台控制

```sh
# 首次写入用户级 lattice.service 并通过 systemd 启用与启动后台（排空上限默认 60s）
lattice daemon install
lattice daemon install --drain-timeout 30s

# systemd 用户服务日常管理与无人登录驻留
systemctl --user status lattice
systemctl --user restart lattice
loginctl enable-linger

# 经本地 IPC 查询状态或排空停止后台（人类可读中文输出或 --json）
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
systemctl --user stop lattice
cargo install --locked --path crates/lattice-cli
cargo install --locked --path crates/lattice-daemon
systemctl --user start lattice

# 从自动拉起后台的旧版本首次升级：先停旧后台，再安装或启动 systemd 服务
/path/to/old/lattice daemon stop
lattice daemon install
lattice --json daemon status
```

## 结果

### 后台控制

| 结果 | 含义与下一步 |
| --- | --- |
| `lattice daemon install` 退出 `0` | 已原子写入 `${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/lattice.service`（`TimeoutStopSec` 为排空上限加 30 秒），并完成 `systemctl --user daemon-reload` 与按该文件绝对路径的 `enable --now`；unit 不在运行中的 user manager 的搜索路径里（例如命令行的 `XDG_CONFIG_HOME` 与 manager 不同）时，systemd 会先把它链接进 manager 的配置目录 |
| `lattice daemon install` 退出 `2` | 同目录缺失当前有效用户可执行的普通文件 `lattice-daemon`、传入了不支持的 `--json`、`--drain-timeout` 无效或加 30 秒后溢出，或可执行路径 / 状态根含无法写入 systemd unit 的字符；未写 unit 且未调用 `systemctl` |
| `lattice daemon install` 退出 `1`（写入 unit 失败） | 创建 `systemd/user` 目录、创建临时文件、写入、同步或重命名失败，unit 不一定写出，且未调用 `systemctl`；核对目录权限与磁盘空间后重试 |
| `lattice daemon install` 退出 `1`（`daemon-reload` 失败） | unit 已写出，但 `systemctl --user daemon-reload` 失败；不执行 `enable --now`，也不探测后台；核对用户级 systemd 会话与错误输出后重试 |
| `lattice daemon install` 退出 `1`（`enable --now` 失败） | unit 已写出且已完成 `daemon-reload`，探测一次状态根上的后台并按三种情况给出提示与对端 pid：已有后台应答时报出对端 pid 并提示先执行 `lattice daemon stop` 再执行 `systemctl --user start lattice`；对端协议版本不兼容时保留版本诊断与对端 pid；无后台应答时提示执行 `systemctl --user status lattice` |
| `unconfirmed_operations == 0` | 人类可读输出中文后台状态摘要与组件区块（无活跃或未挂载组件时为 `组件：无`），`--json` 输出 `epoch`、`stopping`、`owners`、`unconfirmed_operations` 与 `components`；`stop` 后状态根已释放 |
| `unconfirmed_operations != 0` | 有未确认收尾的操作（`daemon stop` 退出 `1` 并在标准错误提示）；查相关 Session |
| `stopping` / `not_running` | 锁未放开或断连（`daemon status` 复核），或无后台运行 |
| `lattice-daemon` | 手动运行后台守护进程（`--state-root` 必填且须为绝对路径，其下用 `lattice/`；`--drain-timeout` 默认 `60s`，参数错误退出 `2`，状态根被占用退出 `3` 并附对端进程号）；按 Ctrl-C（`SIGINT`）或发 `SIGTERM` 进入有界停机，排空完成退出 `0`，排空超时或排空中再次收到信号退出 `4`；故障注入限于隔离目录 |

### 升级与输出字段适配

| 结果 | 含义与下一步 |
| --- | --- |
| `incompatible_backend` | 本地 IPC 协议版本（当前为 `68`）与运行中后台不一致；用 `systemctl --user stop lattice` 或旧二进制 `lattice daemon stop` 停止旧后台后再启动新版本 |
| `transport`（零字节断开） | 更早后台或崩溃断开；核对存活与版本 |
| 拒绝格式版本较旧的库或 `*.git` 旧库 | 原库不变并列路径；存档或删除须获授权 |

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
