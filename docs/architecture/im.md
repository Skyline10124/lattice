---
status: active
---

# IM 网关

> 类型：架构合同
> 上位依据：[项目宪章](../governance/charter.md)、[工程规范](../development/engineering.md)
> 决策记录：[ADR 0011](../adr/0011-im-gateway-inside-backend.md)、[ADR 0013](../adr/0013-user-image-input.md)

本合同规定本地后台进程内 [IM 网关](../reference/glossary.md)组件从宿主托管（`lattice-daemon` 与 `lattice-server`）、平台凭据存储（[`im_auth`](../../crates/lattice-server/src/im_auth.rs)）、IPC 与 CLI 管理（`lattice im`、`lattice daemon status`）到外部消息平台私聊文本、单图与多图相册对话接入（[`lattice-im`](../../crates/lattice-im/src/lib.rs)）、受控媒体下载、[台账](../reference/glossary.md)相位、并行恢复、命令语义、流式草稿与回复切分的完整规则。当前首个支持平台为 Telegram。

## 概念

| 概念 | 含义 | 状态持有者 | 生命周期 |
| --- | --- | --- | --- |
| 托管组件槽位 | 后台按组件名（`"im"`）维护的挂载状态、运行实例句柄、停止通知与合并重新挂载协调器 | `lattice-server`（`local/components.rs`） | 随 `lattice_server::serve` 启动完成初始挂载，运行期响应 IPC 或 `inotify` 重新挂载，停机第 3 步统一收尾 |
| IM 网关实例 | 经只读配置与凭据检查后构造并运行的平台接入循环（`PreparedImGateway`） | `lattice-im`（`gateway.rs`） | 构造阶段只读本地文件；运行阶段驱动轮询、相册收集、媒体下载、恢复与回复，收到停止通知或后台进入停机时排空全部子任务后退出 |
| IM 配置目录监视 | 针对 `<config_dir>/im` 目录的异步 `inotify` 监视器与递增代次状态 | `lattice-daemon`（`im.rs`、`im_watch.rs`） | 每次 `ImHost::prepare` 成功确认私有目录后建立新代次监视并替换旧描述符；目录被删除或移走时按代次标记失效，待下次重新挂载恢复 |
| IM 配置文件 | 配置目录下的 `im/config.toml`，声明平台启用表、白名单、默认工作区与默认 agent | 用户维护，`prepare_im_gateway` 只读解析 | 在构造 `PreparedImGateway` 时读取并校验，运行实例内部保持不变 |
| 平台凭据存储 | 配置目录下的私有文件 `im/auth.json`，保存各平台访问密钥（`ImToken`） | `lattice-server`（`ImAuthStore`） | 按需打开读取、进程内串行写锁加 `0600` 原子替换更新；内存缓冲用后清零 |
| 身份键 | 带小写平台前缀的强类型标识：`SenderKey`、`ChatKey`、`InboundId`（如 `tg:12345`） | `lattice-im`（`identity.rs`） | 平台原始数值标识在平台模块边界转换为身份键，贯穿授权、相册收集、绑定、台账与草稿映射 |
| 相册收集器 | 按 `ChatKey` 维护的当前开放 `media_group_id` 多图相册缓冲与最近 64 个已闭合 `media_group_id` 集合（`AlbumCollector`） | `lattice-im`（`album.rs`） | 仅驻留当前实例轮询任务内存；收集期间不确认平台更新，满足静默到期、同对话新事件到达或满 10 项上限时闭合入队，窗口内停机交由平台重投整组相册 |
| 台账 | `<state-root>/lattice/im/ledger` 私有 JSON 文件（`version == 2`），记录 chat 当前绑定的 `SessionId` 与入站事件相位（`Registered`、`Accepted`、`Merged`、`DeliveredTurn`、`DeliveredReset`） | `lattice-im`（`Ledger`） | 实例启动 `run` 时打开并清理超过 `24h + 5m` 重投窗口且关联轮次已终态的条目；运行中每次状态变化经写锁串行原子替换落盘并更新短锁只读快照 |
| 平台确认游标 | 已接收 `update_id` 的有序状态与连续已确认前缀推进器（`OffsetTracker`） | `lattice-im`（`telegram.rs`） | 仅驻留当前实例内存；支持单条与整组相册批量确认，新实例首次 `getUpdates` 不传 offset，交由台账与内存去重吸收重投 |
| 活跃草稿映射 | 当前实例提交的在途轮次 `(ChatKey, draft_id) -> TurnId` 登记表与单 chat 草稿节流表 | `lattice-im`（`reply.rs`） | 仅驻留当前实例内存；随单轮收尾自动移除，重新挂载或重启后的旧轮次不再推送流式草稿 |

## 组件与职责

| 组件 | 持有的状态 | 执行的规则 | 对外接口 |
| --- | --- | --- | --- |
| [`components.rs`](../../crates/lattice-server/src/local/components.rs)（`lattice-server`） | 已注册组件的运行句柄、当前公开状态（`Disabled` / `Mounted` / `Unmounted`）、正在进行与排队的重新挂载槽位 | 在后台就绪后并发启动初始挂载并与后续合并重新挂载串行；旧实例未退出前不启动新实例，单实例收尾上限 10 秒（超时 `abort`）；配置无效且旧实例仍存活时保留旧实例并报告 `UsingPreviousConfiguration`；以线程局部组件 panic 作用域抑制原始 panic 载荷进入 `stderr` 日志并经监督任务隔离错误与 panic；与 `Backend::begin_stop` 共享准入停机锁边界，停机第 1 步原子阻断新实例启动并唤醒在途重新挂载返回 `Stopping`，第 3 步在严格共享 10 秒上限内并行等待全部组件任务退出 | `ComponentRegistration`、`ComponentBuildOutcome`、`PreparedComponent`、`ComponentStop`、`ComponentRemountHandle`、`ScopedComponentFuture`、`install_component_panic_hook`、`scoped_component_future`、`with_component_panic_scope`；IPC `ImAuthSet`、`ImAuthRemove`、`ImAuthStatus`、`ImReload` 及 `DaemonStatus.components` |
| [`im.rs`](../../crates/lattice-daemon/src/im.rs)、[`im_watch.rs`](../../crates/lattice-daemon/src/im_watch.rs)（`lattice-daemon`） | `ImHost`（`StateLayout`、启动期 `HOME` 快照、当前 `inotify` 监视状态与递增代次） | `prepare` 核验 `HOME` 为绝对路径，经 `PrivateDirectory` 以 `0700` 创建缺失的 `~/.lattice` 与 `~/.lattice/im`（已有路径权限或类型不合规绝不自动修改），每次成功准备时建立新代次的 `rustix::fs::inotify` 异步监视并复核目录 `(st_dev, st_ino)` 身份以替换旧描述符；仅当 `config.toml` 触发 `CLOSE_WRITE` 或 `MOVED_TO` 时调用 `ComponentRemountHandle::remount`；收到 `DELETE_SELF` / `MOVE_SELF` / `IGNORED` 时按代次标记当前监视失效，不自动重建被删目录 | 向 `lattice_server::serve` 注册 `"im"` 组件 |
| [`im_auth.rs`](../../crates/lattice-server/src/im_auth.rs)（`lattice-server`） | 配置目录路径句柄 `ImAuthStore`、零化秘密包装 `ImToken` 与平台状态 `ImPlatformCredentialState` | 经 `PrivateDirectory` 核验配置目录、`im/`（`0700`）与 `auth.json`（`0600`、1 MiB 上限）；严格解析 `version == 1`、拒绝未知字段/重复键/尾随字节；校验 `"telegram"` 平台名与 `1..=16_384` 字节无控制字符密钥；进程内写锁保护重读与原子替换；错误消息零秘密回显 | `ImAuthStore`（`new`、`get`、`status`、`set`、`remove`）、`ImToken`（`as_str`，不实现 `Debug` 或 `Display`）、`ImPlatformCredentialState`、`ImAuthError`、`TELEGRAM_PLATFORM` |
| [`commands/im.rs`](../../crates/lattice-cli/src/commands/im.rs)、[`commands/daemon.rs`](../../crates/lattice-cli/src/commands/daemon.rs)（`lattice-cli`） | 无持久状态 | 提供全中文 `lattice im` 命令组（`auth set <平台>`、`auth remove <平台>`、`auth status`、`reload`）与 `lattice daemon status` 组件区块；`im auth set` 先本地校验平台名（不回显非法值）再经无回显终端或限长管道读取凭据；在 `main.rs` 拦截 `im` 命令解析错误，杜绝命令行误传秘密被回显 | CLI `lattice im ...`、`lattice daemon status`（文本与 `--json`） |
| [`config.rs`](../../crates/lattice-im/src/config.rs) | 已校验的 `TelegramGatewayConfig`（白名单集合、规范化工作区路径、`AgentName`、`ImToken`） | 只读读取 `im/config.toml` 与 `ImAuthStore`，区分未启用（`Disabled`）、缺凭据（`Unavailable`）、配置或文件违规（`InvalidConfiguration`）与就绪实例（`Prepared`）；构造阶段不启动任务、不解析 DNS、不联网、不访问台账 | `prepare_im_gateway`、`ImBuildOutcome` |
| [`identity.rs`](../../crates/lattice-im/src/identity.rs)、[`ledger.rs`](../../crates/lattice-im/src/ledger.rs) | `SenderKey`、`ChatKey`、`InboundId`，以及异步写锁串行落盘加同步短锁持有 `Arc<LedgerData>` 快照的 `Ledger`（`bindings`、`inbound` 有序映射与单代发布门闩 `LedgerPublicationLatch`） | 平台前缀身份键编解码校验；`<state-root>/lattice/im/ledger` 的 `0700`/`0600` 严格读取、校验 `version == 2` 与原子替换（损坏或版本不符直接报错而不覆盖）；单条消息登记 `Registered` 或多图相册在同一次原子替换中登记最小 `update_id` 首条 `Registered` 与其余分片 `Merged`；启动期及运行期逼近 16 MiB 上限时清理早于当前时间超过 `24h + 5m` 的 `DeliveredTurn`、`DeliveredReset` 以及关联轮次已不在 `Registered`/`Accepted` 的 `Merged` 条目，并在经 `PrivateDirectory::replace_guarded` 持发布门闩替换前强制校验写出体积不超过 16 MiB 上限；读操作仅在短锁内克隆引用计数，不跨越异步等待或磁盘 I/O | 模块内向网关公开，根模块导出 `ImGatewayError` |
| [`album.rs`](../../crates/lattice-im/src/album.rs)、[`media.rs`](../../crates/lattice-im/src/media.rs) | `AlbumCollector`（每对话当前开放相册缓冲与最近 64 个已闭合 `media_group_id` 队列）；`media.rs` 无持久状态 | 按数值 `update_id` 升序收集 `media_group_id` 分片，在 `600ms` 静默窗口到期、同对话收到非本相册消息/命令/新相册或收满 `10` 项 Telegram 单相册上限时闭合（迟到同组分片固定提示 `LATE_ALBUM_FRAGMENT_REPLY`）；对单图与已闭合相册先执行零网络静态快筛（拦截不支持媒体、超过 `8` 张、已知 `0` 字节或单图超过 `3.75 MiB`、已知总和超过 `10 MiB`、拼接说明文字超过 `2 MiB`），再按序受控下载（`429` 遵守 `retry_after` 与 `5xx`/传输错误最多重试 3 次，停机中断立即退出且不确认平台，`400`/`404` 不重试），核验每张非空、整组累计原始字节不超过 `10 MiB` 与前 12 字节文件头魔数（`ImageMediaType::sniff`），组装 `[Image_1, ..., Image_k]` 及可选末尾 `Text` | 模块内向网关公开 |
| [`command.rs`](../../crates/lattice-im/src/command.rs)、[`usage.rs`](../../crates/lattice-im/src/usage.rs)、[`redact.rs`](../../crates/lattice-im/src/redact.rs) | 无持久状态；`ImRedactor` 持有按长度降序排列的已知非空密钥列表 | 私聊加 `allowed_senders` 白名单判定；纯文本消息精确识别 `/new`、`/cancel`、`/status`、`/usage`、`/help` 与 `/start`（`HELP_REPLY` 末行提示发送文字或可附说明文字的图片），独立不支持媒体固定提示 `UNSUPPORTED_MEDIA_REPLY`；识别后台因模型不支持图片输入、累计超过 `32 MiB` 本地请求体预算或远端 `HTTP 413` 的拒绝，保留脱敏中文原因并统一补充减少图片、发送 `/new` 开启新会话或在终端执行 `lattice session rewind` 的引导；直出 `SessionUsage` 的 `path_total` 与 `session_total` 生成四行用量摘要；`reqwest::Error` 先调用 `without_url()` 再替换任意长度非空密钥为 `[REDACTED]` 并折叠为单行诊断 | 模块内向网关与平台客户端公开 |
| [`telegram.rs`](../../crates/lattice-im/src/telegram.rs)、[`telegram_http.rs`](../../crates/lattice-im/src/telegram_http.rs) | `OffsetTracker` 与缓存的已固定公网地址 `reqwest::Client` | 仅订阅 `["message", "stopped_message_generation"]`；将纯文本 `text`、压缩图片 `photo`（取最后一项最高分辨率 `PhotoSize`）、声明 MIME 忽略大小写与参数后属于四种受支持图片格式的未压缩 `document`（先拦截 `animation.mime_type` 非 `image/gif` 的服务端转码动图，其余归为 `Unsupported`）、可选 `caption`、`media_group_id` 与停止生成事件转为内部载荷；支持批量 `acknowledge(&[InboundId])` 推进连续前缀；运行期在阻塞线程池执行 `net_guard::validate_base_url` 与 `resolve_public_socket_addrs` 固定连接 `https://api.telegram.org`，仅信任 `Service::tls_roots_der()` 根证书，禁用代理与重定向；`download_file` 先经 `POST /bot<token>/getFile` 核验 `file_size` 与合法相对路径 `file_path`（拒绝以 `/` 开头、空路径段、`.`、`..`、`\`、`?`、`#` 与控制字符），再以 `GET /file/bot<token>/<file_path>` 有界流式读取至多 `max_bytes + 1` 字节（超限立即中断拒绝）；分类处理 `429`、`5xx` 与 `400`/`403`/`404` | 模块内通过唯一的测试替身接口 `TelegramIo` 供网关调用 |
| [`reply.rs`](../../crates/lattice-im/src/reply.rs)、[`gateway.rs`](../../crates/lattice-im/src/gateway.rs) | 随机正整数草稿登记表 `ActiveDrafts`、`ChatDraftThrottleMap`、`AlbumCollector`、在途去重集合 `in_flight_inbound`、分发点同步定序的 `ChatAdmissionQueues` 与受监督子任务集合 | 启动期同步建立遗留 `Registered` 的同 chat 计数屏障并逐条并行恢复（仅 `Registered` 与 `Accepted` 驱动恢复，`Merged` 不单独触发恢复；准入判定与回复投递解耦）；轮询中驱动相册 `600ms` 静默闭合、同对话提前闭合、`10` 项上限闭合、每对话 `64` 个近期已闭合 ID 迟到分片提示，以及存在未确认相册或在途重投过滤为空时的 `100ms` 空轮询节流；在单对话准入 worker 中完成图片受控下载核验后再整组原子登记台账并批量确认；驱动单 chat 每秒 1 条纯文本草稿节流与 25 秒保活、末尾 4096 字符截取、失败结局说明（含图片与请求体大小拒绝引导）拼接、不丢换行的 4096 字符三级切分、收发器阻塞任务排空及子任务异常向宿主传播 | `PreparedImGateway::run` |

### 宿主托管、重新挂载与 CLI 接口

1. **启动目录准备与 `inotify` 监视**：
   - 守护进程绑定监听并宣告就绪后，同步占位并并发启动已注册组件的初始挂载驱动（不阻塞 `api.sock` 连接接受与停机信号处理；初始挂载进行期间到达的重新挂载请求排入同一槽位合并队列）。`ImHost::prepare` 检查 `HOME` 为绝对路径后，确保 `<config_dir>`（`$HOME/.lattice`）与 `<config_dir>/im` 存在（缺失新建为 `0700`；已存在但属主、非符号链接真实目录或 `mode & 0o077 == 0` 不满足时拒绝自动修改权限并返回安全中文原因，使 `"im"` 列为 `Unmounted`）。
   - 目录核验通过后，`ImHost::prepare` 每次均建立新代次的基于 `rustix::fs::inotify`（`NONBLOCK | CLOEXEC`）与 `AsyncFd` 的目录监视（建立后复核目录 `(st_dev, st_ino)` 与 `0700` 私有属性，并替换旧代次监视，防止同 inode 移走移回后旧描述符队列中迟到的失效事件撤销新监视）。
   - 仅当文件名恰好为 `config.toml` 且事件包含 `CLOSE_WRITE` 或 `MOVED_TO` 时自动请求重新挂载 `"im"`；`auth.json`、临时文件、尚未关闭的写入以及删除 `config.toml`（`DELETE` / `MOVED_FROM`）均不触发自动重新挂载。若 `<config_dir>/im` 被删除或移走（收到 `DELETE_SELF` / `MOVE_SELF` / `IGNORED`），按代次标记当前监视失效，不自动重建目录；后续执行 `lattice im reload` 或 `lattice im auth set|remove` 调用 `prepare` 时重建 `0700` 目录并恢复监视。
2. **合并重新挂载、保留旧实例与故障隔离**：
   - 每次挂载先在阻塞池调用 `prepare`（失败直接按 `InvalidConfiguration` 处理），再调用只读 `build`（`prepare_im_gateway`）。
   - 若 `build` 返回 `InvalidConfiguration` 且当前已有仍在运行的网关实例，保留该实例继续运行，组件状态记为 `Mounted { configuration_error: Some(reason) }`，回执返回 `ImMountResult::UsingPreviousConfiguration { reason }`；若当前无存活实例，则状态记为 `Unmounted { reason }`，回执返回 `ImMountResult::Unmounted { reason }`。
   - 若 `build` 返回 `Disabled` 或 `Unavailable`（如删除了 `config.toml` 或移除了平台凭据），或返回新的 `Prepared` 实例，而当前存在运行实例，则先发送组件级停止信号并等待旧实例任务完全退出（至多 10 秒，超时调用 `abort()`），旧实例退出后才按新结果更新为不展示（`Disabled`）、`Unmounted` 或启动新实例并标记 `Mounted`。运行中的后台会话轮次归 `Service` 所有，重新挂载期间后台 `epoch` 不变、在途轮次不中断，新实例启动后通过台账接手未投递轮次的终态观察与最终回复。
   - 同一组件重新挂载进行期间到达的后续重新挂载请求不会并发执行，而是合并为当前轮结束后的同一次重新挂载，共享那一次重新读取磁盘配置与凭据后的回执。
   - 组件构造阶段或运行中的网关及其子任务若返回 `Err(reason)` 或发生 panic，宿主通过组件 panic 作用域抑制原始 panic 载荷进入 `stderr` 日志，并由监督任务捕获异常将组件状态原子切换为 `Unmounted { reason }`（绝不拼入原始 panic 载荷），后台进程继续正常服务；此后经 `lattice im reload` 或配置修复即可重新挂载。
3. **后台停机收尾**：
   - 后台进入停机第 1 步时在 `Backend::admission` 同一锁临界区内同步推进组件管理器停机转换（先于 `HostLifecycle::on_stopping` 与对外停机通知）：向所有运行中组件发出停止信号，唤醒正在等待旧实例退出的重新挂载及合并等待者立即返回 `Stopping` 错误，并与组件最终启动/状态提交共享该锁边界，确保停机生效后绝不再启动任何新实例。
   - 在途轮次排空（第 2 步）结束后，停机第 3 步在单一共享 10 秒截止时间内并行等待所有仍在运行或退场中的组件任务结束（无活跃组件任务时零等待），超时对剩余任务调用 `abort()` 后不再额外计时等待。
4. **IPC、状态展示与 `lattice im` 退出码**：
   - `DaemonStatus.components` 按 `name` 字典序返回非 `Disabled` 组件列表；IM 未启用（`Disabled`）时不包含 `"im"` 条目，`lattice daemon status` 文本模式在无活跃/未挂载组件时输出 `组件：无`，非空时逐行缩进渲染 `im：已挂载`、`im：已挂载；仍按上一份配置运行；<原因>` 或 `im：未挂载；<原因>`。
   - `lattice im` 命令语义与退出码：

| 命令 | 行为与文本输出 | `--json` 输出类型 | 退出码 |
| --- | --- | --- | --- |
| `lattice im auth set <平台>` | 先校验 `<平台>`（仅支持 `telegram`，非法不回显），再从无回显终端或管道读取密钥写入 `im/auth.json` 并重新挂载；输出 `<平台> 凭据已保存` 与 `IM：<挂载结果>`（停机期间已完成落盘时挂载结果为说明未重挂原因的 `Unmounted`） | `ImAuthSetReply` | 保存成功 `0`；非法平台/密钥/参数 `2`；后台未运行/写入失败/传输错误 `1` |
| `lattice im auth remove <平台>` | 校验 `<平台>` 后从 `im/auth.json` 移除凭据并重新挂载；输出 `<平台> 凭据已删除`（或 `凭据未配置`）与 `IM：<挂载结果>`（停机期间已完成更新时挂载结果为说明未重挂原因的 `Unmounted`） | `ImAuthRemoveReply` | 执行成功 `0`；非法平台/参数 `2`；后台未运行/写入失败/传输错误 `1` |
| `lattice im auth status` | 查询各支持平台在 `im/auth.json` 中的凭据状态，逐行输出 `telegram：已配置`、`telegram：未配置` 或 `telegram：无效；<原因>` | `ImAuthStatus` | 查询成功 `0`；参数错误 `2`；后台未运行/传输错误 `1` |
| `lattice im reload` | 触发 `"im"` 重新挂载并输出 `IM：<挂载结果>` | `ImMountResult` | `Mounted` 或 `Disabled` 为 `0`；`UsingPreviousConfiguration` 或 `Unmounted` 为 `1`；参数错误 `2` |

### 配置与凭据核验

`prepare_im_gateway(config_dir, layout, service)` 按以下顺序只读检查本地文件：

```text
读取 <config_dir>/im/config.toml（经 PrivateDirectory，上限 1 MiB）:
    if 配置目录、im/ 或 config.toml 不存在:
        return Disabled
    if 目录或文件属主不符、为符号链接、非普通文件、权限含组或其他用户位（mode & 0o077 != 0）、超限或 UTF-8/TOML 无效:
        return InvalidConfiguration
    if TOML 顶层含 [telegram] 之外的未知表，或 [telegram] 含未知字段:
        return InvalidConfiguration
    if 未定义 [telegram] 表:
        return Disabled
    核验 [telegram]:
        allowed_senders: Vec<i64>（允许为空数组，空数组拒绝全部发送者）
        workspace: 绝对路径，经 canonicalize 解析且目标必须为真实目录
        agent: 经 AgentName::parse 校验名称语法（不在构造期检查 profile 文件是否存在）
        任一项不满足 -> return InvalidConfiguration
    经 ImAuthStore::get("telegram") 读取 <config_dir>/im/auth.json:
        Ok(None) -> return Unavailable（缺少 Telegram 平台凭据）
        Err(_)   -> return InvalidConfiguration
        Ok(Some(token)) -> return Prepared(PreparedImGateway)
```

`im/auth.json` 采用固定 JSON 结构 `{"version": 1, "platforms": {"telegram": "<密钥>"}}`：

- 读取缺失目录或文件返回空结果且不创建任何路径；写入时若配置目录或 `im/` 不存在则以 `0700` 创建，已有目录只核验不自动修改权限，凭据文件以 `0600` 同目录临时文件加 `fsync` 原子替换。
- 反序列化拒绝未知字段、重复平台键、尾随字节与非 `1` 版本号；密钥须为 `1..=16_384` 字节且不含控制字符；解析错误与平台名校验错误均不回显输入原文或文件片段。

### 授权与内置命令

- **授权过滤**：仅处理私聊（Telegram `chat.type == "private"`）且 `SenderKey` 命中 `allowed_senders` 的消息（无论文本或媒体）。非私聊、缺少发送者字段或不在白名单内的消息不下载文件、不写台账、不建会话、不发回复，收到后立即向 `OffsetTracker` 确认以免阻塞后续 offset 推进。
- **入站消息载荷识别**：

| 入站消息形态 | 识别与路由规则 |
| --- | --- |
| 纯文本消息（有 `message.text`，无媒体） | 精确匹配内置斜杠命令表；未命中命令表的文本作为单条 `[Text]` 用户消息进入单对话准入队列 |
| 压缩图片（`message.photo` 非空） | 取 `photo` 数组最后一项（最高分辨率 `PhotoSize`）的 `file_id`、可选 `file_size` 与可选 `caption`；无 `media_group_id` 直接排入单对话准入队列，带 `media_group_id` 进入相册收集器 |
| 未压缩图片文档（`message.document` 存在，且 `mime_type` 忽略参数与大小写后为 `image/png`、`image/jpeg`、`image/webp`、`image/gif`；排除 `animation.mime_type` 非 `image/gif` 的服务端转码动图） | 取 `document` 的 `file_id`、可选 `file_size` 与可选 `caption`（实际图片格式在下载后按 12 字节文件头魔数核验）；按是否携带 `media_group_id` 进入单图准入或相册收集 |
| 其他媒体或空消息（`sticker`、转码为 MP4 的 `animation`、`video`、`voice`、`audio`、其他 `document` 等） | 无 `media_group_id` 时不下载、不写台账、不建会话，立即确认并回复 `只支持文本和图片消息（PNG、JPEG、WebP、GIF）`；带 `media_group_id` 时进入相册收集器，在相册闭合后整组拒绝一次并回复同一说明 |

- **命令识别与说明文字边界**：内置命令仅对纯文本消息匹配；图片或相册附带的 `caption`（即使写成 `/new` 或 `/cancel`）一律作为该轮的用户说明文字，不触发内置命令。纯文本中 `/new`、`/cancel`、`/status`、`/usage`、`/help` 仅在整条文本精确匹配时识别为命令；`/start` 在整条文本为 `/start` 或 `/start` 后跟空白与参数时识别为命令并忽略参数。除上述六项外的全部文本（含 `/unknown`、`/model` 等其他斜杠文本）均作为普通用户消息提交给模型。

| 命令 | 是否写台账 | 行为与回复规则 |
| --- | --- | --- |
| `/help`、`/start` | 否 | 立即确认，回复固定的六行中文命令说明（5 条命令说明与末行 `直接发送文字或图片（可附说明文字）即向当前会话提交一轮对话。`），不创建会话、不修改绑定 |
| `/new` | 是（`DeliveredReset`） | 若当前对话存在开放相册缓冲则先闭合入队，随后按单对话队列顺序在同一次台账原子写入中移除当前 `ChatKey` 的会话绑定并登记该 `InboundId` 为已投递，确认平台并回复已清除绑定；在途旧会话轮次若有草稿继续按其 `draft_id` 独立收尾 |
| `/cancel` | 否 | 若当前对话存在开放相册缓冲则先闭合入队；命令本身立即确认并查询当前绑定会话的 `Service::head`：无绑定、会话不存在（含会话库未初始化）或 `owner == false` 回复 `没有进行中的轮`；有活跃 `turn_id` 时调用 `Service::cancel(turn_id)`（无论该轮由 IM、CLI 或 TUI 发起），按返回 `Ok(true)`、`Ok(false)`、`Err(_)` 分别回复 `已取消，等待这一轮结束`、`这一轮已结束或不在执行中`、`取消结果未知，可发 /status 查看` |
| `/status` | 否 | 若当前对话存在开放相册缓冲则先闭合入队；命令本身立即确认并查询当前绑定会话的 `Service::head`，无绑定或不存在时回复 `当前没有绑定的会话`，存在时分行展示会话身份、执行状态、agent、模型与推理档位 |
| `/usage` | 否 | 若当前对话存在开放相册缓冲则先闭合入队；命令本身立即确认并查询当前绑定会话的 `Service::usage`，无绑定或不存在时回复 `当前没有绑定的会话`，存在时直出 `path_total` 与 `session_total` 原值生成严格四行摘要（会话与当前路径轮数、按首次出现顺序去重的 `<provider>/<model>` 列表、当前路径合计、全会话合计；计数未知为 `-`、溢出附 ` (overflow)`，缓存命中率仅在有尝试且输入与缓存读取均完整未溢出且输入大于 `0` 时按四舍五入百分比显示且不钳制 100%，费用未知显示 `$?`、小于 `$0.01` 的正数显示 `<$0.01`、估算前缀 `≈`，不完整合计行末附 `（不完整）`） |

## 流程

```mermaid
sequenceDiagram
    participant TG as Telegram Bot API
    participant IM as lattice-im 网关
    participant LG as im/ledger 台账 (v2)
    participant SV as lattice-server Service

    TG->>IM: getUpdates 返回私聊文本、单图或 media_group_id 相册分片
    IM->>IM: 校验私聊与 allowed_senders；相册分片暂不确认，满足 600ms 静默/新事件/10 项上限时闭合
    IM->>IM: 在分发点同步排入单对话 FIFO 准入队列
    opt 单图或已闭合相册
        IM->>IM: 静态快筛（格式、<= 8 张、单张 <= 3.75 MiB、总计 <= 10 MiB、说明 <= 2 MiB）
        IM->>TG: getFile 相对路径核验 + GET /file/bot<token>/<file_path> 有界流式下载 + 魔数核验
    end
    IM->>LG: 原子写入首条 Registered 与相册其余分片 Merged（无绑定时写入新 SessionId）
    IM->>TG: TelegramIo.acknowledge(&all_inbound_ids)
    alt 首次绑定或绑定草稿会话已不存在
        IM->>SV: configure(New(session_id), workspace, agent)
        IM->>SV: submit(session_id, turn_id, workspace, parts, expected_settings=None)
    else 已有绑定会话
        IM->>SV: submit(session_id, turn_id, workspace, parts, expected_settings=None)
    end
    SV-->>IM: Ok(InProcessTurn)
    IM->>LG: 原子更新首条为 Accepted
    IM->>IM: 分配随机正整数 draft_id 登记 TurnId，推进单对话队列处理下一条
    loop 轮次执行期间（发送前后及请求在途均持续监听终态）
        SV-->>IM: InProcessTurn::next_draft() 纯文本快照（终态立即中断在途慢草稿）
        IM->>TG: sendMessageDraft（单 chat 1 秒节流、重试清空、成功按发起时刻 25 秒保活与失败退避分离、末尾 4096 字符、can_stop=true）
    end
    SV-->>IM: InProcessTurn::outcome()
    IM->>IM: 拼接非 Completed 结局说明（含图片与请求体大小拒绝引导）并按 4096 字符三级切分
    IM->>TG: 按序调用 sendMessage 发送全部切片
    IM->>LG: 原子更新首条为 DeliveredTurn（记录 delivered_at）
```

关键分支处理：

1. **相册收集、闭合判定与空轮询节流**：
   - 授权私聊消息携带 `media_group_id` 时，先查台账（已落账的 `DeliveredTurn`、`Accepted`、`Registered` 或 `Merged` 立即静默确认跳过，不扰动当前开放相册）；若当前对话存在另一个 `media_group_id` 的开放相册则先将其闭合入队，再查该对话内存中最近 `64` 个已闭合 `media_group_id` 集合（FIFO 淘汰）：若该新分片的 `media_group_id` 命中该对话的近期已闭合集合，说明分片延迟到达，不下载、不写台账，立即确认并回复 `该相册部分图片延迟到达，未并入上一轮`。
   - 其余分片加入 `(ChatKey, media_group_id)` 开放缓冲并全程持有 `InFlightGuard`，收集期间不调用 `acknowledge`（窗口内停机或崩溃后由 Telegram 完整重投整组相册）。满足以下任一条件时按数值 `update_id` 升序闭合相册、记入近期已闭合集合并排入单对话 FIFO 准入队列：
     - **静默窗口到期（`600ms`）**：每次收到同对话同 `media_group_id` 新分片将截止时刻重置为 `now + 600ms`，到期无新分片即闭合；
     - **同对话收到下一条非本相册事件**：同一 `ChatKey` 收到无 `media_group_id` 的消息/命令或另一个 `media_group_id` 的分片时，先闭合当前开放相册入队，再处理新事件（`stopped_message_generation` 不闭合开放相册）；
     - **达到 Telegram 单相册协议上限（`10` 项）**：收满 `10` 条同组分片立即闭合（不在 `8` 张处截断，使 9～10 张超限相册在下一步作为整组被拒绝一次）。
   - 当一轮 `getUpdates` 批次过滤本实例已确认与在途 `InboundId` 后无任何新更新，且原批次非空或存在待闭合开放相册时，轮询循环在监听停止信号的前提下休眠 `min(100ms, 距最近开放相册截止剩余时长)` 节流，防止未确认分片重投引发空转紧循环。
2. **受控文件下载、魔数核验与有序片段组装**：
   - 单图或已闭合相册在单对话准入 worker 队首依次执行：
     - **下载前静态快筛**：若含不支持媒体、图片张数超过 `MAX_USER_IMAGES_PER_TURN`（`8` 张）、任一已知 `file_size == 0` 或超过 `MAX_IMAGE_BYTES`（`3.75 MiB`）、已知 `file_size` 总和超过 `MAX_USER_IMAGE_TOTAL_BYTES`（`10 MiB`），或非空 `caption` 以 `"\n"` 拼接后超过 `MAX_USER_TEXT_BYTES`（`2 MiB`），不发起网络下载、不写台账、不建草稿会话，一次性确认全部关联 `InboundId` 并回复具体中文原因；
     - **受控流式下载与有界重试**：每张图片以 `min(MAX_IMAGE_BYTES, 10 MiB - 已下载总字节)` 为上限调用 `TelegramIo::download_file`。`TelegramHttpClient` 复用固定公网地址与后台启动期根证书的客户端，先请求 `POST /bot<token>/getFile` 核验 `file_size` 与非空相对路径 `file_path`（拒绝以 `/` 开头、空段、`.`、`..`、`\`、`?`、`#` 与控制字符），再请求 `GET /file/bot<token>/<file_path>` 流式读取至多 `max_bytes + 1` 字节（超限立即中断拒绝）。遇到 `429`（遵守 `retry_after`）、`5xx` 或传输瞬断时在监听停机信号的前提下有界退避重试（最多重试 `3` 次，共至多 `4` 次尝试）；停机中断时直接退出且不确认平台更新；重试耗尽或遇 `400`/`404` 不可重试错误时确认全部关联 `InboundId` 并回复脱敏下载失败说明；
     - **字节与魔数核验**：核验每张下载字节非空、整组累计原始字节不超过 `10 MiB` 且前 12 字节通过 `ImageMediaType::sniff`；未通过时一次性确认全部关联 `InboundId`、不写台账、不建会话并回复原因。核验通过后按 `update_id` 升序排列全部 `UserInputPart::Image`，若存在非空 `caption`（不做空白裁剪，按 `update_id` 升序以 `"\n"` 拼接）则追加一个末尾 `UserInputPart::Text`。
3. **台账原子登记、草稿会话重建、忙拒绝与后台图片/大小拒绝引导**：
   - 单条消息登记 `InboundId -> Registered { chat, session, turn }`；多图相册在**同一次台账原子替换**中将最小 `update_id` 对应的首条登记为 `Registered { chat, session, turn }`（无绑定时同时写入 `chat -> session`），将其余分片登记为 `Merged { chat, session, turn, delivered_at: now }`。台账落盘成功后先一次性调用 `acknowledge(&all_ids)` 确认平台，再释放整组 `InFlightGuard`。
   - 已有绑定的 chat 调用 `Service::submit` 返回 `NotFound`（或会话库尚未初始化）时，网关复用已写入台账的同一个 `SessionId`、`TurnId` 与完整 `parts` 执行 `configure(New)`（幂等忽略 `SessionExists`）加 `submit`。
   - 同一 `ChatKey` 的普通消息、相册与 `/new` 在分发点同步排入单对话 FIFO 队列串行推进到得出准入结果为止（台账写入遇临时 I/O 失败时守住队首退避重试，绝不放行后续事件越过未落账队首），不同 `ChatKey` 并行；得出准入结果后立即放行单对话队列，观察与回复在受跟踪子任务中进行，执行期间到达的下一条消息进入 `Service::submit` 收到 `Busy` 时回复 `正在忙，可发 /cancel` 并将首条台账记为 `DeliveredTurn`。
   - 后台因目标模型不支持图片输入、累计历史超过 `32 MiB` 本地请求体预算或远端返回 `HTTP 413` 拒绝时，无论发生在准入预检（`LocalErrorKind::BadRequest`）、实时轮次失败（`outcome.diagnostic` 或 `summary.notice`）还是重挂/重启恢复补发（从持久会话库读取 `summary.notice`），IM 均返回脱敏中文原因，并统一提示减少图片、发送 `/new` 开启新会话或在终端执行 `lattice session rewind`。
4. **准入结果不明与启动期并行恢复**：
   - `configure` 或 `submit` 返回 `Stopping` 或携带 `recovery` 时，保留首条台账 `Registered` 相位并继续持有该 chat 准入锁，转入按 `TurnId` 轮询 `Service::turn(turn_id)` 的找回循环。
   - 实例启动 `run` 时，先打开台账并清理过期终态条目，再同步为每条遗留 `Registered` 条目累增所属 `ChatKey` 的未决启动屏障计数，并为每条遗留 `Registered` 与 `Accepted` 各自派生独立的并行恢复任务（`Merged` 条目不单独派生恢复任务），随后**不等待恢复完成立即开始 `getUpdates` 轮询**，保证旧轮次未结束时 `/cancel`、`/status`、`/usage` 等命令与其他 chat 的消息照常响应。
   - `Registered` 找回：`TurnLookup::Accepted` 转 `Accepted` 后立即释放该条目的启动屏障并在独立任务中观察补发最终回复；`TurnLookup::InFlight` 或临时存储错误保留 `Registered` 退避重试；`NotFound`（含会话库未初始化）立即释放启动屏障，并在独立任务中回复 `该消息未被执行`、发送成功后记 `DeliveredTurn`。
   - `Accepted` 找回：调用 `Service::observe(session_id, turn_id)` 等待 `outcome()`（不补发流式草稿），发送最终回复后记 `DeliveredTurn`；临时错误保留 `Accepted` 退避重试。
5. **草稿停止按钮**：收到私聊 `stopped_message_generation` 时立即确认更新且不闭合当前开放相册，在内存 `ActiveDrafts` 中按 `(ChatKey, draft_id)` 查找在途 `TurnId`；命中则调用 `Service::cancel(turn_id)` 并按 `/cancel` 同一文案回复，未命中（轮次已收尾或跨实例重挂）不发多余消息。
6. **回复切分与发送错误处理**：
   - 最终回复取 `InProcessTurnOutcome.assistant_text`；若终态不是 `Completed`，在末尾附一行脱敏后的中文结局说明（正文与结局均为空时用简短兜底文本避免空消息）。总长度超过 4096 个 Unicode 字符时，优先按段落边界（保留 `\n\n`）、次按行边界（保留 `\n`）、最后按 4096 字符边界无损切分为多条消息按序发送（拼接严格还原原文），不切碎多字节字符。
   - `sendMessage` 与 `getUpdates` 遇到 HTTP `429` 时完整遵守服务端 `retry_after`（本地指数退避上限仅约束无 `retry_after` 的限流及其他可重试错误）并在监听停止信号的前提下退避重试；`getUpdates` 遇到非限流错误时按指数退避重试（退避期间照常按截止时刻闭合到期相册）；仅 `sendMessage` 遇到 HTTP `400` 或 `403` 时视为平台拒收终态，记录告警日志、不再重试，并将关联首条台账标记为 `DeliveredTurn`（`sendMessage` 的其余非限流错误按指数退避重试）。
7. **确认游标与优雅收尾**：
   - 写台账事件（文本消息、核验通过的单图/相册、`/new`）在台账落盘后确认；静态快筛或下载核验被拒的图片/相册在拒绝时一次性确认；其余事件收到即确认。确认调用始终发生在释放 `InFlightGuard` 之前，且单对话 worker 出队后再次复核平台确认状态与台账记录，杜绝拒绝或落账边界的重投竞态。`OffsetTracker` 仅弹出队首连续已确认前缀并将下一轮 offset 设为最后弹出 `update_id + 1`。
   - 收到外部 `stop` 或 `Service::stopping()` 通知时，广播内部停止信号中断所有长轮询、相册等待、下载重试与发送退避，等待轮询循环、`JoinSet` 中全部恢复/准入/草稿/台账写入子任务以及 `TelegramIo::drain` 中的在途阻塞解析任务完全退出后 `run` 才返回；即使 `run` 被宿主强制中止，生命周期守卫 `LedgerPublicationGuard` 也会在丢弃时同步封闭发布门闩，阻止阻塞线程池中滞留的旧台账写入覆盖新实例数据。若轮询或任一受监督子任务发生 panic，先广播停止并排空同代子任务，再向宿主重新抛出 panic 供组件层隔离。

## 状态

```mermaid
stateDiagram-v2
    [*] --> Registered: 文本、单图或相册首条分片原子写入台账（首次同时绑定 SessionId）
    [*] --> Merged: 相册其余分片随首条在同一次原子写入中落账
    [*] --> DeliveredReset: /new 原子清除绑定并写入台账
    Registered --> Accepted: submit 成功或恢复查询 TurnLookup::Accepted
    Registered --> DeliveredTurn: submit 明确拒绝，或恢复查询 NotFound 并回复未执行（或 400/403 拒收）
    Accepted --> DeliveredTurn: 最终回复全部切片发送成功，或遭遇 400/403 平台拒收
    Merged --> [*]: 窗口清理（delivered_at 早于当前时间超过 24h + 5m 且关联 turn 已不在 Registered/Accepted）
    DeliveredTurn --> [*]: 窗口清理（delivered_at 早于当前时间超过 24h + 5m）
    DeliveredReset --> [*]: 窗口清理（delivered_at 早于当前时间超过 24h + 5m）
```

台账文件（`version == 2`）只保存格式版本号、`bindings`（`ChatKey -> SessionId`）与 `inbound`（`InboundId -> InboundRecord`），不保存用户消息正文、图片原始字节、图片说明文字、助手回复正文、平台密钥、Telegram offset 或 `draft_id`。

## 保证

| 保证 | 适用范围 | 成立条件 | 依据或验证 |
| --- | --- | --- | --- |
| 配置改坏或组件崩溃不中断后台会话与已有可用网关 | 守护进程运行期全部重新挂载与组件异常 | `InvalidConfiguration` 且旧实例存活时保留旧实例继续运行；组件返回错误或 panic 时由监督任务捕获并转为 `Unmounted`（不泄露原始 panic 载荷），旧实例未退出前绝不并行启动新实例 | `local::components_tests`、`gateway_tests::gateway_panic_is_isolated_in_daemon_status_and_recovers_on_im_reload`、`gateway_tests::hosted_gateway_subtask_panic_never_leaks_payload_to_stderr` |
| 同实例重投去重：不重复下载、不重复回复、不重复提交 | 同一网关实例运行期收到的全部入站更新（含图片拒绝与落账边界） | 同一实例内依靠 `in_flight_inbound`、先 `acknowledge` 后释放 `InFlightGuard`、`OffsetTracker` 与单对话 worker 出队复核，消除拒绝或落账边界的重投竞态 | `gateway_tests::deduplicates_inbound_id_preserves_single_session_and_handles_new_and_non_ledger_events`、`gateway_tests::rejected_image_redelivery_race_never_redownloads_replies_twice_or_creates_turn`、`gateway_tests::late_fragment_of_closed_album_closes_different_open_album_before_late_reply` |
| 跨实例重投去重：已落账事件不重复提交轮次，旧 `/new` 重投不清掉后续新会话绑定 | 跨重启或重挂在 Telegram `24h + 5m` 重投窗口内的文本、核验通过的单图/相册与 `/new`（未落账的静态快筛/下载拒绝不写入台账，仅在平台 `offset` 越过该 `update_id` 后不再重投） | 依靠 `<state-root>/lattice/im/ledger` 持久台账去重：文本、核验通过的单图/相册（同一次原子落盘写入首条 `Registered` 与其余 `Merged`）及 `/new` 在原子落盘后才确认平台；窗口清理保留 `24h + 5m` 终态及关联在途轮次的 `Merged` | `ledger::tests`、`gateway_tests::deduplicates_inbound_id_preserves_single_session_and_handles_new_and_non_ledger_events`、`gateway_tests::replies_late_fragment_notice_for_new_update_id_and_silently_acks_redelivered_merged_id`、`gateway_tests::preserves_unclosed_album_offset_across_shutdown_and_recovers_registered_plus_merged_once` |
| 受控媒体下载不越界、不留下空草稿会话且停机中断不丢更新 | 全部私聊图片与相册下载及核验路径 | 先静态快筛，再经固定公网地址客户端校验 `getFile` 相对路径并有界流式读取 `max_bytes + 1` 字节，魔数核验全部通过后才写台账并调用 `configure(New)`；收集窗口内或下载重试期间停机不调用 `acknowledge` | `media::tests`、`telegram_http::tests`、`gateway_tests::rejects_disguised_empty_oversized_and_invalid_path_images_without_ledger_or_draft_session_and_accepts_exact_max_bytes`、`gateway_tests::preserves_unclosed_album_offset_across_shutdown_and_recovers_registered_plus_merged_once` |
| 崩溃或重挂后不丢失已接受轮次的最终回复（含 `HTTP 413` 等失败结局引导），也不阻塞新命令轮询 | 遗留 `Registered` 与 `Accepted` 条目及在途重新挂载 | 启动时同步建立同 chat 准入屏障后异步恢复并立即启动 `getUpdates`；重新挂载保持后台 `epoch` 不变，新实例从持久会话库读取终态与 `summary.notice` 补发回复 | `gateway_tests::recovers_all_ledger_phases_without_blocking_polling_and_orders_same_chat_after_registered`、`gateway_tests::reload_mid_turn_preserves_epoch_and_new_gateway_delivers_final_reply`、`gateway_tests::delivers_http_413_guidance_both_live_and_via_recovered_summary_notice` |
| 机器人凭据与非法平台输入不出现在任何错误、日志、CLI 回显或用户回复中 | `im_auth`、`lattice-cli` 与 `lattice-im`（含 `getFile` 与 `/file/bot<token>/...` 下载路径） | CLI 先本地拒绝非法平台并拦截参数解析错误，终端秘密输入关闭回显；`ImToken` 无 `Debug`/`Display` 且释放清零，`reqwest::Error` 先 `without_url()` 再经无长度门槛的 `ImRedactor` 替换 | `im_auth::tests`、`commands::im::tests`、`redact::tests`、`telegram_http::tests`、`fast_im_cli` |
| 流式草稿不含隐藏推理且同一 chat 每秒至多发送 1 条草稿 | 当前实例提交的在途轮次 | `InProcessTurn` 只提取纯文本语义段，`ChatDraftThrottleMap` 按 `ChatKey` 共享 1 秒节流与 25 秒保活并截取末尾 4096 字符 | `reply::tests`、`gateway_tests::full_turn_lifecycle_streams_clean_draft_and_stop_button_cancels_active_turn` |
| 出站 HTTPS 请求不走代理、不跟随重定向且固定连接已校验公网地址 | `TelegramHttpClient` 全部 Bot API 与文件下载请求 | 运行期在构建或重建 HTTP 客户端时经 `net_guard` 校验 `https://api.telegram.org` 并通过 `resolve_to_addrs` 固定公网地址，仅注入后台启动期根证书 | `telegram_http.rs` 实现与 `telegram_http::tests` |

## 范围之外

- 会话树、Step 节点、图片内容寻址表 `BLOBS`、调用槽位与会话设置的持久权威由 `lattice-session` 与 `lattice-server` 维护（见[会话与 Turn](session/README.md) 与 [私有存储与持久化](storage.md)）；台账不保存对话正文或图片字节，CLI 与 TUI 可在同一含图会话上直接续发。
- 模型输入模态能力、五种协议含图流式请求编码、`32 MiB` 本地请求体预算、远端 `HTTP 413` 分类、供应商凭据与用量计费规则由 `lattice-llm` 维护（见[模型接入](llm/README.md)）；`lattice-im` 不直接依赖 `lattice-llm`。
- 工具目录冻结与调用执行由 `lattice-server` 注入的 `ToolExecutor` 负责。
- IM 网关不向外部平台发送图片附件回复，也不处理群组/频道消息及贴纸、转码为 MP4 的动图、视频、语音、音频与非图片文档。
- systemd user service 安装、后台实例单例锁与 TUI 交互分别由 `lattice-cli`、`lattice-server` 与 `lattice-tui` 维护（见 [Crate 职责与依赖边界](crates.md) 与 [TUI 交互与视觉](tui/README.md)）。

## 验证

| 检查项 | 命令 |
| --- | --- |
| IM 凭据存储、组件托管状态机与 IPC 测试 | `cargo test -p lattice-server --lib im_auth`、`cargo test -p lattice-server --lib components` |
| IM 网关配置、相册收集、受控下载、台账 v2、命令、拒绝引导、草稿、切分、跨前端 `BLOBS` 回放与宿主重挂集成测试 | `cargo test -p lattice-im` |
| 守护进程启动 `0700` 目录准备与 `inotify` 监视恢复测试 | `cargo test -p lattice-daemon` |
| CLI 中文 `lattice im` 与 `daemon status` 渲染及秘密零回显测试 | `TMPDIR=/tmp cargo test -p lattice-cli` |
| 黑盒端到端闭环（[IM 组件与 CLI 场景卡](../development/scenarios/harness/im-component-and-cli.md)） | `TMPDIR=/tmp cargo test -p lattice-system-harness --test fast_im_cli` |
| 依赖边界、线上形状快照与文档门禁 | `env -u LATTICE_UPDATE_WIRE_SHAPE cargo test -p lattice-ipc wire_shape`、`cargo xtask crate-boundary`、`cargo xtask docs-check` |
