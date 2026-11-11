---
status: active
---

# 实现模块与公开操作

> 所属领域：[会话与 Turn](README.md)

以下将前面的角色对应到代码模块，展开调用入口和内部协作。`lattice-api` 负责跨领域协调，把 `ToolExecutor` 工具执行接缝、模型网关和会话库写入接入 Turn；适用边界见[范围与禁止事项](concurrency.md#范围与禁止事项)。

| 模块 | 具体职责 |
| --- | --- |
| `lattice-api::local::LocalClient` / `LocalTurn` | `client.rs` 封装连接与分帧，交付 `configure`、视图与只读查询；`operation.rs` 的具体 LocalTurn 管理提交、取消回执与退出交接 |
| `lattice-api::local::serve` | 持有实例锁和 socket、接收并核验连接、管理连接任务，并在 `configure(New)` 后触发后台草稿清理；实例退出时释放资源 |
| `Backend` / `Owner` | Backend 管理准入、草稿清理、会话库句柄、视图及本进程诊断，为每个观察者保存视图的推送状态；Owner 持有已接管操作的内存槽位（Turn 执行带身份、取消标志、回执、监督信息和当前尝试的段列表；`configure`、`rewind` 与草稿清理为无 Turn 身份占位）。任务脱离请求连接运行，监督者等待操作收尾后才移除 Owner |
| `lattice-api::configure` | 解析并校验会话设置：加载指定 agent 的 profile 或以当前会话设置为基准，查询模型可用档位并执行档位规则，构造新设置与档位调整回执 |
| `lattice-api::SessionApi` | 列表、日志、退回、对话条目与历史查询的领域入口，向后台返回会话事实和对话内容 |
| `lattice-api::TurnCoordinator`、`ToolExecutor` 及执行核心 | 唯一持久 Turn 协调者，持有全 Turn 执行锁，接受前经注入的 `ToolExecutor::open_turn` 冻结本轮工具目录（生产默认 `NoTools`），从会话设置冻结输入，推进模型循环、调用执行、Step 与 Turn 关闭；会话库写入经 `lattice-session` 的领域接口完成 |
| `lattice-agent::ModelStepDriver` | 接收冻结请求和对话历史，驱动一次 provider 请求，向协调层返回类型化尝试结果，并发送非阻塞观察回调 |
| `lattice-llm` | 请求网关：提供无凭据的模型档位能力查询（`model_supported_efforts`），统一判定可用性，`GatewayTurn::prepare` 完成提交准入与模型及档位复核，每次请求按 provider `id` 与模型 `id` 解析配置、取得凭据，完成 provider wire 请求、流事件适配和对话投影 |
| `lattice-conversation` | 维护模型选择值（`ProviderId`、`ModelId`、`ModelRef`）、推理档位 `Effort`、有序消息、ToolCall typestate、调用结果与结构化 `ToolError`、内容摘要、尝试诊断和 usage 等纯数据 |
| `lattice-session` | 维护独立于存储机制的会话树领域类型（`model/`：`AgentName`、`ProjectPath`、`TurnTimeout`、`RetryPolicy`），并拥有全局会话库 `lattice.redb`：Session 记录（含 `ProjectPath` 项目绑定）、会话树、会话设置（`SessionSettings` 与 `SettingsVersion`）、配置表、TurnId 索引与 InvocationId 索引；提供带类型的事务操作、由阶段给出结论与读取校验 |

公开操作按用户要完成的工作划分：

| 用途与操作 | 输入及前提 | 输出与效果 |
| --- | --- | --- |
| 配置会话：`configure` | `New(SessionId)` 或 `Existing(SessionId)`、规范工作区 `workspace`（IPC 中为字节数组），以及可选的 `agent`、`model`、`effort`；`New` 必须给出 `agent` | 新建草稿会话并写入初始会话设置，或核对项目绑定后更新已有会话的会话设置并刷新 `updated_at`；返回 `ConfigureReply { head, adjustment }`（`adjustment` 为 `Option<EffortAdjustment { from, to }>`），观察者收到新的会话头。详情见[会话设置](settings.md) |
| 查看会话：`list`、`log` | 无，或 SessionId | `list` 列出全部会话（草稿会话照常出现在列表中），每行给出 SessionId、项目绑定 `ProjectPath`（`--json` 中为原始字节数组，人类可读输出为有损文本）、当前节点序号、节点数、更新时间，以及最后一轮未关闭时它的 TurnId 和后台是否正在执行它；`log` 先给出项目绑定（同上，`--json` 为原始字节数组，人类可读输出为有损文本）、当前位置、当前 agent 与未关闭的最后一轮，再以树形列出全部节点（含不在当前分叉上的节点），标出当前位置，并在每个 Turn 的首节点注明 TurnId 与 `TurnSummary` |
| 观察会话：`watch` | SessionId | 推送会话头、当前路径上最近几轮的对话条目、之后变化的条目与生成中的文字，见[观察与重连](recovery.md#观察与重连)；草稿会话与当前位置为根节点的会话同样可以观察（初始只有会话头），`configure` 成功后观察者收到新的会话头。会话不存在时返回 NotFound |
| 查看会话头：`head` | SessionId | 返回与视图开头相同的会话头，完整字段为：`session_id`（会话身份）、`position`（当前节点序号）、`agent`（当前 agent，取自会话设置）、`model?`（会话设置中的模型 `id`）、`provider?`（会话设置中的 provider `id`）、`effort?`（会话设置中的推理档位）、`settings_version`（当前设置版本）、`turn_id?`（当前位置所属的那一轮；草稿会话与 `rewind` 回到根节点的会话均为空，因此不能用 `turn_id` 为空判断一个会话是不是草稿会话）、`owner`（`turn_id` 所示那一轮有没有 Owner）、`diagnostic?`（后台已知的执行诊断）、`path_replaced`（单次查询固定为 `false`；观察流中因 `rewind` 替换当前路径时为 `true`）。不读条目，不占观察许可；遇到同会话无 Turn 身份的操作在执行时等待其结束后读取。会话不存在时返回 NotFound |
| 查看一个 Turn：`turn` | TurnId | 返回这一轮的 `TurnSummary` 与全部 Step 节点；请求仍在准入时返回在途，没有这个请求时返回 NotFound。接受结果不明时用它找回 |
| 提交：`submit` | 已有会话的 SessionId、TurnId、`input`、规范工作区 `workspace`（IPC 中为字节数组），以及可选的 `expected_settings` | 后台分两段检查。登记准入时，在准入锁内依次检查：TurnId 正被在途请求使用，返回 Conflict（`turn_id_used`）；后台正在停机，返回 Stopping；全局 Owner 总数已达上限 32，返回 Busy。登记之后依次检查：TurnId 已在 TurnId 索引中，返回 Conflict（`turn_id_used`）；`workspace` 不是存在的规范绝对路径，返回 BadRequest；会话不存在，返回 NotFound；项目绑定不一致，返回 Conflict（`binding_mismatch`）；同一会话已有 Owner，返回 Busy；输入超过 2 MiB，返回 BadRequest；`SessionExecutionLock` 被占用，返回 Busy；给出了 `expected_settings` 且与当前设置版本不等，返回 Conflict（`settings_changed`）；最后检查网关配置、模型（含凭据）与本轮工具目录。接受事务写入时再核对一次绑定与设置版本，成功后返回 LocalTurn，见[请求接受](acceptance.md#请求接受)与[请求身份与找回](recovery.md#请求身份与找回) |
| 查看用量：`usage` | SessionId | 当前路径上逐轮的用量与费用估算，给出当前路径合计和全会话合计（含 rewind 丢掉的分支）；用量口径与费用规则见[模型接入合同](../llm/usage.md#用量缓存与费用) |
| 模型候选：`models` | `ModelCandidateQuery`：`List { provider: Option<ProviderId>, ready_only: bool }` 或 `Target(ModelRef)` | `List` 给出候选及各自的上下文窗口、输出上限、可用档位、价格与 `availability`，过滤 deprecated 与账号目录外的 `chatgpt` 模型；`ready_only` 为真时只返回 `ready` 候选且跳过不可用 provider 的模型与警告。`Target` 返回指定模型的单项候选（含 deprecated、目录外或无元数据的模型）及其 `availability`。合并只读当前全局配置与元数据；`List` 范围包含已登录且未被跳过的 `chatgpt` 且缺目录时先异步补拉，`Target` 保持纯本地，见[模型候选](../llm/models.md#模型候选) |
| 凭据与元数据：`auth` 与 `models_refresh` 各命令 | 见[凭据命令](../llm/credentials.md#凭据命令) | 录入与删除 key、SIWC 登录与登出、provider 可用性状态；立即刷新 models.dev 与已登录账号的模型目录。两来源分别发布，部分失败返回失败来源、原因及已更新部分 |
| 读取更早的对话：`history` | SessionId、节点 `before` 与轮数（1 到 100，超出返回 BadRequest） | 沿 `before` 的父链往回取完整的若干轮，条目与视图中的相同，见[观察与重连](recovery.md#观察与重连) |
| 退回：`rewind` | SessionId、目标 NodeId 与规范工作区 `workspace`（IPC 中为字节数组） | 核对项目绑定 `ProjectPath`（错误诊断中的路径为有损文本）后，把当前位置移到树上任意已有节点；之后的提交从那里分叉，原分支保留在树上。不修改会话设置，也不修改工作区文件 |
| 请求取消：`LocalTurn::cancel`，或独立命令的 `LocalClient::cancel` | TurnId | 句柄同步登记意图并管理在途回执；独立命令返回后台接管结果，持久结局另行查询 |
| 停止后台：`stop` | 当前本地后台 | 关闭新准入并等待已接管操作收尾，socket 撤下、`owner.lock` 可取得后返回停止时已知状态；回执后限时内锁仍未放开报 Stopping |

CLI 的 `lattice session` 子命令为 `send`、`rewind`、`list`、`log`、`turn`、`usage` 和 `cancel`，顶层 `lattice run` 是 `session send` 的别名；凭据与模型的命令 `lattice auth`、`lattice models` 见[模型接入合同](../llm/credentials.md#凭据命令)。`session usage <SESSION_ID>` 逐轮列出用量与费用估算并给出合计，支持 `--json`。`session turn` 查看单个 Turn 的详情，请求仍在准入时报告在途，接受结果不明时用它找回；`--follow` 遇到请求仍在准入时等准入结束，不在途中退出，已接受则继续观察这一轮直至结束，没有这个请求则报告 NotFound，退出 2。`session cancel <SESSION_ID>` 取消该会话当前正在执行的一轮：CLI 用 `head` 查出会话头所示那一轮的 TurnId，再按 TurnId 取消，并输出该 TurnId；没有正在执行的一轮时报告无可取消。已接管、未接管或没有可取消的一轮时退出 0；取消回复丢失、结果不明时，输出带原 TurnId 的取消记录，状态为 `unknown`，退出 1；会话不存在退出 2。

`session send`（`--model <provider>/<model>` 与 `--effort <档位>` 选择模型与推理档位；新建会话时 `--agent` 必填，已有会话可选）按以下顺序执行：

1. 带 `--session` 时先解析会话前缀，不带时生成新的 SessionId；随后生成 TurnId，输出 `submit` 的 `sending` 记录，再从当前目录解析规范工作区（工作区解析前收到 SIGINT 则输出 `submit` 的 `not_accepted` 回执并退出 1；工作区无效则输出 `submit` 的 `rejected` 回执）。因此进程无论在哪一步中断，都能按第一条记录中的 TurnId 查询结果；中断在提交发出之前时，查询返回 NotFound。
2. 决定是否先执行 `configure`：
   - 不带 `--session`：执行 `configure(New)`；
   - 带 `--session`，且给出了 `--agent`、`--model` 或 `--effort` 中的任何一个：执行 `configure(Existing)`；
   - 带 `--session`，三者都没给：跳过 `configure`。
3. 执行了 `configure` 时，按结果继续：

   | 结果 | CLI 的输出与后续 |
   | --- | --- |
   | `configured` | `--json` 输出 `configure` 的 `configured` 记录（人类可读模式在档位自动调整时打印一行提示），随后继续提交，`expected_settings` 为空 |
   | `rejected` | 先输出 `configure` 的 `rejected` 记录，再以同一个错误输出 `submit` 的 `rejected` 回执；退出码按提交被拒的规则 |
   | `unknown` | 先输出 `configure` 的 `unknown` 记录，再输出 `submit` 的 `not_accepted` 回执；退出码为 1 |

4. `configure` 只发一次，结果不明时按上表的 `unknown` 处理；重跑命令是安全的，见[configure 的找回](recovery.md#configure-的找回)。

`session send` 与 `session turn --follow` 的 `--json` 输出 JSONL，只含所提交或所跟的那一轮，共有八种记录：`submit`（发出请求前一条写明 TurnId 与 SessionId，结束时一条写明回执结果）、`configure`（会话配置结果：`configured`、`rejected` 或 `unknown`）、`cancel`（取消回执）、`head`（会话头）、`entry`（对话条目，读者按 `node` 覆盖）、`text`（生成中的文字）、`connection` 与 `error`。向已有会话发送且未给出 `--agent`、`--model`、`--effort` 时，或工作区在 `configure` 之前失败与中断时，没有 `configure` 记录；`session turn --follow` 不会配置会话，也不输出 `configure` 记录。结局在本轮最后一个 Step 条目的 `TurnSummary` 中，不另设结束记录。

输出写明身份的 `submit` 之后，这次提交以一条回执结束（进程被杀除外）。回执的 `result` 为 `accepted`（带接受的这一轮）、`rejected`（带拒绝的错误）、`not_accepted` 或 `unknown`（结果不明，可带错误）；`recovered` 表示结果来自按 TurnId 查询。后台的拒绝（包括 `configure` 与提交的配置错误）是 `rejected`。`rejected` 的错误为 BadRequest（含配置拒绝）、NotFound 或 Conflict 时退出 2，为其他种类（如 Busy、Stopping）时退出 1；`configure` 被拒与提交被拒按同一规则。`not_accepted` 表示确定未被接受，有三种来源：按 TurnId 查询为 NotFound，取消发生在请求写出之前、请求没有发出，或前置 `configure` 结果为 `unknown`、提交未发出。`error` 记录只用于不属于这次提交的错误，例如生成身份之前的使用错误、观察时后台回复的错误；`sending` 要写完整的 SessionId，`--session` 的前缀在生成身份之前解析，解析失败也是 `error` 记录。回执为 `not_accepted` 或 `unknown` 时退出 1。

`session turn` 的详情（含 `--json`）给出 `TurnSummary`，并按步骤序号列出该 Turn 的全部 Step 节点：节点序号、`progress` 及其给出的结束方式与原因，每次尝试的 `progress` 及其给出的终态与原因，以及各槽位的 `progress` 及其给出的结果。它不按路径读取对话。命令行按 TurnId 读取一个 Turn 的全部 Step 只有这一个入口；观察中的 Step 节点随视图推送，见[观察与重连](recovery.md#观察与重连)。输出不含冻结配置中的秘密。不带 `--follow` 而请求仍在准入时，`session turn` 输出一行在途说明，`--json` 时输出 `{"status":"in_flight","turn_id":…}`，都退出 0。

`session turn --follow` 所跟的一轮有 Owner 执行时一定在当前位置（执行中 `rewind` 与同会话的新提交都返回 Busy），视图开头就给得到它的条目。这一轮已结束（已关闭，或未关闭而没有 Owner）时，若视图开头给出的条目里有它带 `TurnSummary` 的最后一步，照常输出这一轮的记录；否则（之后又跑了 10 轮以上，退回到这一轮中途，或最后一步已不在当前路径上）只给结局，不给回复正文：按 `session turn` 的详情输出（`--json` 时是同样的一行），退出码按最新结局。

LocalTurn 交付提交与取消的回执。普通退出使用 `detach` 取得最后已知的提交、取消与诊断，或由 Drop 停止本地后续工作；两者都不新增取消。`LocalTurn::turn_id` 给出提交与取消共用的身份。

公开类型与方法见[本地连接与读取](../../../crates/lattice-api/src/local/client.rs)、[LocalTurn](../../../crates/lattice-api/src/local/operation.rs)和[结果类型](../../../crates/lattice-api/src/local/types.rs)，命令用法见[验证操作指南](../../development/validation-guide.md#环境与后台)。

## 凭据命令与私有交换

凭据命令经后台执行，规则见[模型接入合同](../llm/credentials.md#凭据命令)。登录占一个普通连接，不建立 Turn Owner：前端发 `auth_login {provider}`，后台在同一连接交付 `auth_login_authorization {authorization_url}`；前端可以发送 `action=callback {url}` 或 `action=cancel`，后台最后给 `auth_login_result`（signed_in、非敏感状态及目录刷新结果，或 cancelled）或安全错误。凭据保存后拉取目录，目录失败随 signed_in 返回警告，不撤销登录。浏览器直接回调无需前端续消息。新消息与其载荷严格拒绝未知字段，URL 只在私有登录消息中传输。

前端 EOF、取消或传输失败会取消未提交的登录；后台负责必要文件工作的监督收尾。凭据文件发布是成功提交点，发布后断线或迟到取消不回滚，结果不明时用 `auth status` 核验。持续读取任务保存同一连接的半帧进度与响应编号，界面取消一次事件等待不会重建 socket 读取。logout/remove 的结果显式带撤销状态，remove 同时保留是否删除材料的含义。

## 当前路径的用量查询

`usage` 在一个读事务内固定 Session 的当前位置，沿父链到根，按 UserMessage 分组并按路径先后返回。退回轮中某个 Step 时，当前路径只累计该 Step 及其祖先中的尝试。每轮给出 `turn_id`、从 1 开始的路径轮次 `ordinal`、UserMessage 节点上的 `provider` 与 `model`，以及 `usage`；模型身份取自该轮节点。返回对象为 `{ session_id, position, turns, path_total, session_total }`，其中 `position` 是本次读取的位置，逐轮 `usage`、`path_total` 与 `session_total` 均按[尝试计量与完整性](steps.md#关闭-step-与-turn)派生。`path_total` 汇总当前路径；`session_total` 在同一读事务中汇总该会话全部节点的尝试，不沿父链，包含 rewind 后留存的分支，每个节点只计一次。

当前位置为根时，`turns` 为空、`path_total` 为空和，`session_total` 仍包含已有分支；路径止于 UserMessage 或 Reserved Step 时仍保留该轮，没有尝试的空和由 `attempts=0` 表达。Sent 与执行者消失后留下的尝试同样参与汇总。

会话库严格打开并校验成功后，身份解析与用量读取只读取 Session 记录、身份键和会话节点的计量投影，不解码对话正文、冻结配置或连接材料。需要打开会话库时，仍按[存储合同](../storage.md#身份版本与打开)执行全库解码与校验。

`lattice session usage <SESSION_ID>` 的文本逐轮列出五项计数、尝试数与费用，末尾两行为“当前路径合计”和“全会话合计”，分别对应 JSON 的 `path_total` 与 `session_total`。逐轮明细只列当前路径，所以 `session_total` 可能大于逐轮之和。单项计数没有任何尝试报告时显示 `-`，已知零显示 `0`，溢出的小计附 `(overflow)`；金额格只按[费用估算](../llm/usage.md#费用估算)显示确切、估算或未知，不附 `(partial)`。全局 `--json` 输出上述一个结构化对象；读取成功退出 0，身份解析和错误分类沿用其他会话查询。
