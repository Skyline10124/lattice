# lattice-im 工作指引

提供由 `lattice-daemon` 经 `lattice-server` 组件管理器托管的进程内 IM 网关库（当前首个平台为 Telegram 私聊纯文本接入）：只读解析 `im/config.toml` 并经 `lattice-server` 读取 `im/auth.json` 平台凭据，维护 `<state-root>/lattice/im/ledger` 私有台账与滑动窗口清理，通过进程内 `Service` 完成会话绑定、准入提交、崩溃找回、流式纯文本草稿推送、草稿停止按钮取消与 4096 字符三级切分回复。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 根模块最小公开面导出（`prepare_im_gateway`、`ImBuildOutcome`、`PreparedImGateway`、`ImGatewayError`），其余内部子模块全部私有 |
| `src/config.rs` | 只读挂载构造入口 `prepare_im_gateway`：经 `PrivateDirectory` 安全读取 `im/config.toml`，严格校验 `[telegram]` 表（`allowed_senders`、`workspace` 规范化真实目录、`agent` 名称语法）并读取 `ImAuthStore`，区分 `Prepared`、`Disabled`、`Unavailable` 与 `InvalidConfiguration` |
| `src/identity.rs` | 平台前缀身份键（`SenderKey`、`ChatKey`、`InboundId`）及 `SessionId` / `TurnId` 生成辅助 |
| `src/ledger.rs` | 私有台账 `<state-root>/lattice/im/ledger` 严格解析、原子替换、会话绑定与入站相位（`Registered`、`Accepted`、`Delivered`）校验及超过重投窗口条目的启动期清理 |
| `src/command.rs` | 私聊与数值白名单过滤、内置斜杠命令识别（`/new`、`/cancel`、`/status`、`/usage`、`/help`、`/start`）以及不写台账命令的执行与回复格式化 |
| `src/usage.rs` | `/usage` 四行会话用量与费用摘要格式化（直出会话库 `path_total` 与 `session_total`，复用状态栏缓存命中率与费用显示规则） |
| `src/redact.rs` | 无长度门槛的平台凭据脱敏器 `ImRedactor`：`reqwest::Error` 先调用 `without_url()` 剥离请求路径，再替换任意长度已知凭据并折叠截断为单行诊断 |
| `src/telegram.rs` | Telegram 更新解析、`tg:` 身份键转换、连续已确认前缀推进器 `OffsetTracker` 与本包唯一的抽象接口 `TelegramIo` |
| `src/telegram_http.rs` | 固定连接 `https://api.telegram.org` 的真实客户端：运行期在阻塞线程池执行 `net_guard` 校验与公网地址固定、仅信任后台启动期根证书、禁用代理与重定向，并分类处理 `429`、`5xx` 与 `400`/`403` 平台拒收 |
| `src/reply.rs` | 活跃草稿登记表、单对话共享 1 秒草稿节流与 25 秒保活、末尾 4096 字符草稿截取、最终回复结局说明拼接、段落/行/字符三级 4096 切分与可靠重试发送 |
| `src/gateway.rs`、`src/gateway_tests.rs` | 网关主循环、启动期遗留 `Registered` 准入屏障与并行恢复、同对话按序准入、草稿会话被清理后的同身份重建、全部派生子任务的有界优雅收尾，以及联立 `serve_for_test_with_components` 的组件托管、合并重挂、10 秒 `abort` 强拆恢复与 panic 隔离集成测试 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [IM 网关](../../docs/architecture/im.md)、[Crate 职责与依赖边界](../../docs/architecture/crates.md)、[工程规范](../../docs/development/engineering.md) |
| 台账、私有文件权限或状态路径 | [私有存储与持久化](../../docs/architecture/storage.md) |
| 会话绑定、提交准入、取消与找回 | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md) |
| `/usage` 计数、缓存命中率与费用展示 | [用量、缓存与费用](../../docs/architecture/llm/usage.md)、[TUI 布局](../../docs/architecture/tui/layout.md#statusline) |

## 约束

- 内部依赖仅限 `lattice-server`、`lattice-ipc`、`lattice-store` 与 `net-guard`；整个工作区中只有 `lattice-daemon` 允许依赖 `lattice-im`（含 `dev-dependencies`），由 `cargo xtask crate-boundary` 门禁强制检查。
- 构造阶段 `prepare_im_gateway` 只读检查本地配置文件与凭据，严禁在构造阶段启动异步任务、解析 DNS、调用 `net_guard::validate_base_url`、发起网络请求或读写正在被上一代实例使用的台账。
- 本包唯一的 trait 是测试替身 `TelegramIo`；不引入时钟、存储或后台服务抽象 trait，集成测试直接连接真实进程内 `Service` 与临时状态根。
- Telegram 将机器人凭据放在请求 URL 路径中，所有 `reqwest::Error` 必须先调用 `without_url()`，再经 `ImRedactor` 替换全部非空已知凭据后才允许进入日志、诊断或用户回复；台账严禁保存凭据、消息正文、回复正文、轮询游标或草稿编号。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-im` 与 `cargo clippy -p lattice-im --all-targets -- -D warnings` |
| 修改依赖或公开导出 | `cargo xtask crate-boundary` 与 `cargo xtask harness-boundary` |
| 修改文档或工作指引 | `cargo xtask docs-check` |
