# lattice-im 工作指引

提供由 `lattice-daemon` 经 `lattice-server` 组件管理器托管的进程内 IM 网关库（当前首个平台为 Telegram 私聊文本、单图与 `media_group_id` 多图相册接入）：只读解析 `im/config.toml` 并经 `lattice-server` 读取 `im/auth.json` 平台凭据，执行相册静默窗口收集闭合、受控 `getFile` 流式下载与 12 字节文件头魔数核验，维护 `<state-root>/lattice/im/ledger` 私有台账（v2，含相册并入相位 `Merged`）与滑动窗口清理，通过进程内 `Service` 完成会话绑定、准入提交、后台图片/请求体大小拒绝引导、崩溃找回、流式纯文本草稿推送、草稿停止按钮取消与 4096 字符三级切分回复。

## 模块

| 模块 | 负责 |
| --- | --- |
| `src/lib.rs` | 根模块最小公开面导出（`prepare_im_gateway`、`ImBuildOutcome`、`PreparedImGateway`、`ImGatewayError`），其余内部子模块全部私有 |
| `src/config.rs` | 只读挂载构造入口 `prepare_im_gateway`：经 `PrivateDirectory` 安全读取 `im/config.toml`，严格校验 `[telegram]` 表（`allowed_senders`、`workspace` 规范化真实目录、`agent` 名称语法）并读取 `ImAuthStore`，区分 `Prepared`、`Disabled`、`Unavailable` 与 `InvalidConfiguration` |
| `src/identity.rs` | 平台前缀身份键（`SenderKey`、`ChatKey`、`InboundId`）及 `SessionId` / `TurnId` 生成辅助 |
| `src/ledger.rs` | 私有台账 `<state-root>/lattice/im/ledger`（`version == 2`）严格解析、整组原子替换（单条 `Registered` 或相册首条 `Registered` 加其余分片 `Merged`）、读写分离短锁内存快照、会话绑定与入站相位（`Registered`、`Accepted`、`Merged`、`DeliveredTurn`、`DeliveredReset`）校验，以及超过 `24h + 5m` 重投窗口且关联轮次已终态条目的窗口清理 |
| `src/album.rs` | 按对话维护的 `media_group_id` 多图相册收集状态机 `AlbumCollector`：`600ms` 静默窗口重置闭合、同对话新事件提前闭合、`10` 项 Telegram 单相册上限闭合、按数值 `update_id` 升序产出，以及每对话最近 `64` 个已闭合相册 ID 的迟到分片识别（`LATE_ALBUM_FRAGMENT_REPLY`） |
| `src/media.rs` | 单图与已闭合相册共用的零网络静态快筛（格式、张数、单张/总字节与说明文字上限）、监听停机信号的有界退避重试下载（最多重试 3 次，停机中断直接退出且不确认平台）、累计原始字节与 12 字节文件头魔数核验（`ImageMediaType::sniff`），以及有序 `[Image_1, ..., Image_k]` 加可选末尾 `Text` 组装 |
| `src/command.rs` | 私聊与数值白名单过滤、内置斜杠命令识别（`/new`、`/cancel`、`/status`、`/usage`、`/help`、`/start`，含 `HELP_REPLY` 图文发送说明与独立不支持媒体提示 `UNSUPPORTED_MEDIA_REPLY`）、不写台账命令的执行，以及后台错误与图片模态/`32 MiB` 本地请求体预算/远端 `HTTP 413` 拒绝的统一 `/new` 与 `lattice session rewind` 引导格式化 |
| `src/usage.rs` | `/usage` 四行会话用量与费用摘要格式化（直出会话库 `path_total` 与 `session_total`，复用状态栏缓存命中率与费用显示规则） |
| `src/redact.rs` | 无长度门槛的平台凭据脱敏器 `ImRedactor`：`reqwest::Error` 先调用 `without_url()` 剥离请求路径，再替换任意长度已知凭据并折叠截断为单行诊断 |
| `src/telegram.rs` | Telegram 更新解析（纯文本 `text`、最高分辨率压缩图片 `photo`、受支持图片类 `document`、转码 `animation` 动图拦截、可选 `caption` 与 `media_group_id`）、`tg:` 身份键转换、支持批量确认 `acknowledge(&[InboundId])` 的连续前缀推进器 `OffsetTracker` 与本包唯一的抽象接口 `TelegramIo`（含 `download_file`） |
| `src/telegram_http.rs` | 固定连接 `https://api.telegram.org` 的真实客户端：运行期在阻塞线程池执行 `net_guard` 校验与公网地址固定、仅信任后台启动期根证书、禁用代理与重定向，实现 `getFile` 相对路径校验与有界流式 `GET /file/bot<token>/<file_path>` 下载，并分类处理 `429`、`5xx` 与 `400`/`403`/`404` |
| `src/reply.rs` | 活跃草稿登记表、单对话共享 1 秒草稿节流与 25 秒保活、末尾 4096 字符草稿截取、最终回复结局说明拼接（含实时 `diagnostic` 与持久恢复 `summary.notice` 下的图片/请求体大小拒绝引导）、段落/行/字符三级 4096 切分与可靠重试发送 |
| `src/gateway.rs`、`src/gateway_tests.rs` | 网关主循环、相册闭合与 `100ms` 空轮询节流、启动期遗留 `Registered` 准入屏障与并行恢复、同对话按序下载核验与整组原子落账确认、草稿会话被清理后的同身份重建、全部派生子任务的有界优雅收尾，以及联立 `serve_for_test_with_components` 与 `LocalClient` 的组件托管、相册/图片全矩阵、后台拒绝引导、跨前端 `BLOBS` 回放与 panic 隔离集成测试 |

## 先读

| 修改的内容 | 读取 |
| --- | --- |
| 任何改动 | [IM 网关](../../docs/architecture/im.md)、[Crate 职责与依赖边界](../../docs/architecture/crates.md)、[工程规范](../../docs/development/engineering.md) |
| 图片与相册输入、受控下载或拒绝引导 | [ADR 0013 第 8 节](../../docs/adr/0013-user-image-input.md#8-im-网关telegram-私聊图片与相册输入)、[对话模型与跨 Provider 投影](../../docs/architecture/session/conversation.md) |
| 台账、私有文件权限或状态路径 | [私有存储与持久化](../../docs/architecture/storage.md) |
| 会话绑定、提交准入、取消与找回 | [重开、取消与未完成状态](../../docs/architecture/session/recovery.md)、[实现模块与公开操作](../../docs/architecture/session/operations.md) |
| `/usage` 计数、缓存命中率与费用展示 | [用量、缓存与费用](../../docs/architecture/llm/usage.md)、[TUI 布局](../../docs/architecture/tui/layout.md#statusline) |

## 约束

- 内部直接依赖（`dependencies` 与 `build-dependencies`）仅限 `lattice-server`、`lattice-ipc`、`lattice-store` 与 `net-guard`（`dev-dependencies` 允许按需用于跨前端测试）；整个工作区中只有 `lattice-daemon` 允许依赖 `lattice-im`（含 `dev-dependencies`），由 `cargo xtask crate-boundary` 门禁强制检查。
- 构造阶段 `prepare_im_gateway` 只读检查本地配置文件与凭据，严禁在构造阶段启动异步任务、解析 DNS、调用 `net_guard::validate_base_url`、发起网络请求或读写正在被上一代实例使用的台账。
- 本包唯一的 trait 是测试替身 `TelegramIo`；不引入时钟、存储或后台服务抽象 trait，集成测试直接连接真实进程内 `Service` 与临时状态根。
- Telegram 将机器人凭据放在请求 URL 路径（含 `/bot<token>/getFile` 与 `/file/bot<token>/<file_path>`）中，所有 `reqwest::Error` 必须先调用 `without_url()`，再经 `ImRedactor` 替换全部非空已知凭据后才允许进入日志、诊断或用户回复；台账严禁保存凭据、消息正文、图片字节、说明文字、回复正文、轮询游标或草稿编号。

## 验证

| 改动 | 运行 |
| --- | --- |
| 任何改动 | `cargo test -p lattice-im` 与 `cargo clippy -p lattice-im --all-targets -- -D warnings` |
| 修改依赖或公开导出 | `cargo xtask crate-boundary` 与 `cargo xtask harness-boundary` |
| 修改文档或工作指引 | `cargo xtask docs-check` |
