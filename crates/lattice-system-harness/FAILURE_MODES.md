# S2 外部 harness 失败方式

依据：`docs/development/testing.md`、`docs/architecture/session.md`；本次接缝和范围由 `.pi/tmp/r2-s2-plan.md` WP4/§7 指定。记录在测试之前；以下不是覆盖通过声明。

| ID | 前提、失败方式 | 独立失败信号 / 接缝 |
| --- | --- | --- |
| W4-01 | JSONL parser 把畸形 summary、未知/双 outcome、缺失或错误等级 notice 当作成功；事件与 summary 不符；缺少 Step/节点、读取失败被 Completed 掩盖 | 手写三态及拒绝 fixture；公开 parser 对终结事件和快照同样校验；失败样本不得产出成功结果，读取错误独立于业务结局 |
| W4-01b | 接受未知/连接诊断撤销后来已确认结局；未知事件被丢弃；畸形 UTF-8/JSON 丢失原行 | 合法未知事件保留 raw；完整历史成功后可分类 Completed；读取未完成不能单凭结局或 exit0 推导成功；畸形行保留 |
| W4-01c | 同 anchor 的最终 State/snapshot 晚于成功 History 导致误判失败；反之把旧 anchor 的 History 当新 snapshot 已交付 | 同 cut 的状态不撤销已加载正文，不同 cut 不继承成功；direct_observation 真实输出观察到 History 后最终 snapshot，binary=0 而 parser=1 |
| W4-02 | driver 先 session new 或偷偷加 bootstrap Turn，首次 prompt 尚未发送已有 Session；丢失原始命令证据 | 构造 driver 不执行命令，SID 初始 None；首次实际 send 才回填接受 SID；拒绝仍返回完整 CommandOutcome；真实二进制核 provider/Turn 数 |
| W4-03 | 成功证据进入失败目录，清理/EOF/脱敏扫描失败被吞 | ScenarioEvidence 先清理，再独立成功归档；保留原断言和清理错误；成功 bundle 含 binary hash/阶段公开读取/参数 |
| W4-04 | runtime-free 门禁未迁移或失去 production test-* 拒绝 | 前序 xtask 临时 workspace normal/dev 负例已存在；继续保留 |
| W4-05 | 旧产物/未命中屏障/杀错后台被当崩溃通过 | 显式 feature 二进制 hash、marker turn/step/part/transaction 与 PID/组/state root 一致；精确非零目标实际 SIGKILL |
| W4-06 | 私有 redb 查询代替公开验收，旧 checkpoint/文件 rewind 断言残留 | CLI/公开 LTLOCAL3 IPC 验证；仅已有工作区 Git 检查例外；harness-boundary 不弱化 |

- W4-07：分段 fake provider 必须能按原到达顺序交付正文/reasoning/调用交错的合法 SSE；逐帧 gate 不得合并、重排或丢终结帧。先用手工 chunks 精确编码断言，后用于 S2-VERTICAL，不能用 IllegalResponse 名义构造正常回复。
- W4-03b：成功归档仅在断言和后台清理均成功后写入独立的新目录；既有路径拒绝覆盖，脱敏与扫描沿用失败证据。用临时目录及秘密哨兵验证成功记录不进入失败目录。
- W4-03c：marker、公开 IPC 快照与构建身份的结构化附件必须随命令证据一起脱敏/扫描；非法附件名拒绝，不能把 JSON 冒充 PTY 或 CommandOutcome。

## 旧测试承接

- parser 的旧 TurnOutcome 标签对象、Git commit id、checkpoint、Gap 后补屏障专属表示随 R2 退役；其独立风险（畸形结果拒绝、固定快照补足未收到的终结事件、错误不被成功掩盖、未知回执不撤销结局、未知事件保留、UTF-8/字段类型/事件顺序及命令结果保留）迁入 R2 summary/Step/历史完成测试，不保留旧协议兼容解析。
- 文件 Rewind/恢复审计链专属场景按计划删除；正常文件安装、前态、项目 Git/历史可达性仍由 direct_mutation/direct_workspace_history 与 API 历史验收保护；会话退回/分叉/文件不回退由 direct_tree_session 承接（待实施）。
- crash 目标迁移：旧 TurnStarted/ModelAttemptStarted → turn_accepted/request_started；BindingPlanned/reservation/Bound → calls_registered 原子事务；candidate/draft/cursor → invocation_started/invocation_finished，关闭后原子 Step 取代累积 transcript pin。旧 TurnClosing/checkpoint 专属窗口消失；保留已安装文件/history current、原终态不变、usage、无重放/无重复调用、超大成功与取消的独立风险。新矩阵逐项验证对应 redb 停点，S2-VERTICAL 覆盖多调用前序 Terminal/Started/Registered、累计历史、只读重开及写触发关闭；尚未运行的风险不得写成通过。

## 第 2 轮恢复：现行场景迁移

- W4-08：旧 driver/结果接口使 direct_local_turn、direct_failures、direct_mutation、direct_operator、direct_workspace_history、PTY 目标不能编译；入口 check-entry.log 为接口红，不是行为红。迁移保留每个原场景独立风险，用 SessionId/TurnId/NodeId、summary、Step 与 history 取代 Git commit/旧 outcome；不增加 bootstrap Turn。
- direct_local_turn 的原流顺序由有界 provider gate/实时观察场景保护；无 gate 的快响应允许初始快照已经关闭，必须按最终 history 精确校验正文，不能强求订阅前的事件重放。log 改核完整树与 Turn summary；外部文件/Git HEAD、后续 provider 上下文、拒绝不安装仍独立断言。
- direct_failures 的旧 Interrupted 按批准三态改 Failed；SIGINT 的 Cancelled 按现行 CLI 退出2。原缺省失败信号保留，另核最后 Step cause；重试父提交改核新 User 的 parent 为前一 Turn 最后节点。
- 旧 direct_session_isolation 的 branch-list/create、checkout、reset、diff 五项都针对已退役命令；它们共同的跨 Session 不串读、不移动其他 Session、不混上下文风险，由同一真实两 Session 场景核验 log/rewind/分叉/无效节点/Turn 详情与后续 provider 上下文承接，命名分支和 diff 专属输出退出。
- 旧 direct_operator 的 pinned ledger OID 改为一致 read DTO 与全局 ChangeSeq；fetch、todo、混合、recent20、未知 Session/Turn 六个目标保留。recent20 由 tail 终态次序精确比对，不依赖随机 InvocationId 的 query 顺序。新增无 profile 查询及零调用 replay 在同一场景验证。
- 旧 direct_turn_contract 的 branch/status/commit 字段由 list/log/current/summary 与不可变 Step/Turn 详情承接；路由、无凭据接受前拒绝、原消息顺序和缺失 Session 无执行保留。
- direct_background：回执未读、前端真实退出、Busy/跨 Session 并发/stop、慢订阅与大正文、自启竞争、跨项目 endpoint/key/profile 冻结、预算、重启只读/历史 Turn 旁路新 Owner 均保留。旧同 TurnId 在两个 Session 各自执行的期望已被 ADR0006 全局身份取代：新测试要求不同 SID 冲突、原事实及调用数不变；不是降低隔离标准。旧 Gap/补屏障专属事件改快照/完整 History/Finished，不另加兼容解析。
- PTY 保留原5项入口；无 SID 启动后首次接受才经 list 取得 SID，不先造 Session。中途重连必须立即包含旧 delta，再接新 suffix，不再期待“前文可能缺失”；第一次复跑因迁移时误写不存在的 running Turn 文案超时，修为实际 stream 状态，不放宽预算。
- fast_session_cli：退役独立 new、project_branch、status、branch 空输出；创建风险由 first_prompt 和首发真实 Turn 承接，列表、跨进程持久化、文本/JSON、缺库不创建、不存在 SID/缺 profile 拒绝由新查询/树场景承接。不能再通过 new 制造无 Turn Session。
- fast_audit_cli / fast_lancet_cli：缺少全局库按现行 StoreUnavailable/NotInitialized 退出1，非旧 invocation ledger 的 BadRequest/空成功；JSON 精确核库路径及类型，不泄露 workspace。已初始化但无调用的项目继续核 recent=[]、latest_change=0。
- fast_env_boundary 的目录覆盖正反对照改通过真实首发与 fake provider 验证：dotenv 拒绝时无 Accepted/零请求，进程显式覆盖才接受且模型为所选 profile；不是把原安全断言删除。
- release_operator 保留 fetch、Component、11 Turn 混合/22调用/recent20 三组；旧 ledger OID/--at 退役，一致读、ChangeSeq 三次状态、排他空续页与 replay 精确事实承接。只编译，不称 release rollout 已执行。

## S2 计划关联真实场景（实施前登记）

- W4-09：中部 Step 退回丢旧分叉/改旧 Turn、根节点隐式沿用 agent、文件跟着会话回退、NodeId 复用。经真实多步 mutate + 两 agent + rewind 中部/根、后续 provider 上下文和文件字节验证，不读取 redb。
- W4-10：旧 sessions.git 或其他项目 invocations.git 被忽略，新 redb 静默创建。只在隔离状态根造旧 Git fixture，send/list/audit 必须列路径并失败，provider 计数为0，新库不存在。此为现行拒绝策略，不是墓碑测试。

- 已删除无引用的旧 `tests/fixtures/submission_unknown_completed.jsonl` 与 `turn_outcome_tags.json`，不再保存可被误用的旧 Gap/checkpoint 成功样本。未知提交/取消回执、后来成功与畸形终态的独立保护由 `src/session_send_tests.rs` 当前 summary/History fixtures 承接；原数据仍可从 Git 历史取得，不保留兼容解析。

## S4 显示与证据

- S4-01：扩展现有 PTY 分段重连场景为 Text → Reasoning → Text；原前端实时正文、无凭据重连及最终新前端读取必须保持同一连续正文，隐藏 reasoning 不引入换行。真实 provider gate 证明片段窗口，公开 log/turn/audit 与 PTY 帧保留到独立成功目录。
- S4-02：现有两 agent 退回分叉场景补存文本 log/turn、JSON、audit、二进制摘要与复现参数；通过不再仅删除临时状态后留下测试计数。复用已验证的 ScenarioEvidence::finish_success，不改证据格式或清理语义。

## S5 历史保留证据

- S5-H01：旧 Git 会话与 retained 删除后，工作区 mutation 的 current/TurnId/InvocationId 不能丢失。复用两 agent 多步退回场景，补存退回前后 history current 与其提交消息，和原 Turn/slot 身份同包复核；不直接读取 redb。这里仅增加证据采集，当前只前进的隔离回归继续由 runtime history 与 API workspace_history_acceptance 保护，不另造重复场景或墓碑测试。

## S6 矩阵与窗口证据（实施前登记）

- S6-01：rewind 提交后、回执前崩溃，重开不得回到旧 current、改写原 Turn 或丢失分叉；用真实 CLI 移到中部/根、命中提交屏障并 SIGKILL，再从公开 log/turn/history 核验与发送。缺屏障是验证失败，不算崩溃通过。
- S6-02：原矩阵只有可重试失败与请求取消，没有不可重试尝试、已持久 Cancelled 槽位及补尾 Deadline。扩展原提交矩阵并以真实 gated fetch 取消取得槽位终态，核对 live 与 reopen 的有序关闭结果、已发生 mutation、无旧请求/Tool 重放。
- S6-03：关闭事务已提交但事件尚未追加时，订阅错误等待 Owner 或先 Finished 后正文。用显式 feature 调度 gate 挂起事件发布（不持存储/观察锁），订阅必须即时交付 Closed 快照、最后 Step、根路径和 Finished。
- S6-04：慢订阅实际未落出64窗口却被当通过。保持同一公开 IPC 订阅，在服务端读游标前以显式 feature gate 暂停，放行超过64个交错 delta 后恢复，要求新快照与后续事件重建完整内容并与持久 Step 对照；仅断开后重连不代替此窗口。
- S6-05：旧库两者并存未逐路径拒绝或拒绝时修改旧库。扩展原 legacy 场景为单个/并存，按文件清单与字节摘要核旧库不变，缺库查询不创建；没有 Session 的拒绝场景如实记录 log/turn 不适用。
- S6-06：成功 bundle 的复现命令硬编码为别的测试，或未保存阶段参数/状态/树/Turn/audit。复用已有证据机制，逐场景记录精确 test/case、脚本、feature 与构建身份；不改写旧产物。
- S6-07：自动关闭 commit 回滚、提交前未知、提交后未知时，另一个 Session 仍在登记调用后或 Continue 间隙；不得继续 Tool/provider、新输入或 rewind。外部真实二进制以显式 feature 复用存储提交故障点，调度 gate 证明旧 Owner 未退场时不能重开；释放后真实关闭旧句柄、严格重开，公开读取按磁盘事实区分三个结果，不自动关闭其他 Turn。再次关闭成功才继续原写操作；故障不能写用户 Cancelled。保存两 Session 的前后 Turn/log/audit、窗口、进程和 provider 请求。
- S6-07b：排空后严格重开仍失败不能自动 create 或重新准入。外部场景只在自有临时状态根、重开 gate 期间将库文件临时移开（不读取 redb）；公开查询/新提交/rewind 均报 NotInitialized，原文件还原后同一后台仍不可用；重启后才能读原事实。失败期间不产生替代库，inode 与已保存节点保持，归档注明这是打开失败而非磁盘掉电。
- S6-08：只有内存中的取消回执却在崩溃后被当持久 Cancelled。在 Requesting 提交后调度暂停、cancel 返回 true、尚无停止提交时 SIGKILL，查询保持 Running，随后 send 只关闭旧请求为 ProcessExit 并恰好执行新输入；与 S6-02 的持久取消对照。
- S6-09：回执未读的首发/已有 Session ×后台持续/重启四组合必须找回原 Turn，不能重新加载已删除配置、重复接受或多建 Session；旧测试仅首发组合。扩展同一测试保留输入冲突/独立 TurnId 的断言，在公开 IPC 写完整配置后不读取接受帧，记录该连接销毁窗口、原 Turn 和 provider 请求；分别在重启之前或之后首次找回。
- S6-10：跨 Session 并发/Busy/取消接管/stop 及回执丢失场景的成功证据缺失；只增加公开 list/树形 log/turn/audit、参数与窗口附件并调用既有成功归档接口，不另写重复的产品行为测试。
- S6-11：逐提交点必须在带三个调用的多步脚本中验证（接受/首个请求天然尚无前序 Step），第二 Step 停止不能丢失第一 Step。精确选择非槽位的步骤停点，记录真实 step/part；只读 query/subscribe 不关闭，rewind 后逐节点核对调用和结果的身份/方法/次序、前序 Step/终态不变、尾槽原因、唯一最后 end 与 TurnSummary。超大成功仍成功；不把未命中屏障或单调用场景当完整矩阵。先用现有产物观察缺步骤选择红，再扩展仅显式 feature 的选择器；已有关闭语义另以受控变异验证独立失败信号。
- S6-12：两 agent 场景补第二 Turn 实际安装工具后、下一 provider gate 内取消；rewind 中部后不指定 agent 必须继承目标 Turn。退回前/中部/原末端/根的公开 history 精确核路径，再逐项核 provider messages（含完整 ToolCall/Result 投影）、系统提示及工具定义；不存在节点与根无 agent 在拒绝后不改变整树；取消/退回均不回退两个文件及工作区历史。已有业务先于扩展测试，原绿时用受控投影变异证明，不伪称原缺陷红。
- S6-13：观察窗口场景合入流内 server_error 重试、三调用 Step、执行中重连与最终71个交错片段。记录失败尝试已显示正文、新尝试为空且旧正文不进树/请求、工具执行中完整回复仍可重连；同一慢订阅确实落出64窗口后重建，关闭事件前取得多步完整历史。已实现业务用关闭尝试清理/流内可重试性受控变异证明独立红，禁止把脚本时序失败算产品红。
- S6-14：最终历史读取挂起/报错不撤销 Completed，也不清除已显示的流正文；CLI 必须退出1而非成功，服务端不能伪造 Finished。仅显式 feature 按 history anchor.head 控制所有该页读取等待/错误，真实 CLI+PTY+公开 IPC 同时验证；解除错误后同 TUI /reconnect 取得完整正文、无重复助手块，退出不再报告当前正文不可用。旧页迟到替代关系仍由 LocalTurn 局部回归保护，未实测的外部窗口不冒称完成。
- S6-15：同后台跨项目操作员读取在混合生命周期/六种终态时串读、游标按项目而非全局推进、recent重复或超过20。外部脚本组合22调用（含超大成功/拒绝/失败）、真实fetch取消补尾、Started后SIGKILL补Indeterminate；另一项目产生夹在其中的变更。query/tail/replay/status逐身份核对，空过滤页推进全局游标，删除profile后仍可读；公开DTO不得含参数/结果正文。已有实现原绿时以临时项目过滤变异取得独立红，完整撤销后重跑。
- S6-16：跨项目同TurnId重发不得共享原Owner/旧记录或先加载配置；原输入与Session即使相同，workspace绑定不同仍Conflict，原事实与provider次数不变。补到已有全局身份场景并归档，不新增重复测试。
- S6-17：rewind提交屏障扩为三调用两步，恢复后保留每个调用结果配对及原分支；同一慢订阅组合在最终关闭后重启，以旧epoch游标再订阅必须返回新快照与相同完整历史，不重新请求provider。
- S6-18：真实TLS接入使用两套自签CA/localhost证书，同后台两个Session并发provider握手后冻结来源被替换，后续provider与fetch仍仅使用各自冻结根/策略。正式CLI SSL_CERT_FILE/SSL_CERT_DIR来源，不把dotenv信任覆盖当合法配置；公开IPC坏DER在接受前拒绝，跨根provider失败、Tool TLS错误保留效果/失败阶段，空允许列表拒绝且无GET。场景专用Python标准库TLS代理仅包裹既有正式fake provider，证书由隔离openssl命令生成；不安装依赖、不输出私钥，不用HTTP证明TLS。
- S6-19：真实两步/三调用Turn在工具Step后暂停下一模型回复；仅TUI发起的旧anchor2读取成功后挂起交付。放行模型并重连，新anchor3完整正文与Completed先显示，再释放旧页、确认返回marker，前端不得丢新正文或重复旧块。公开分页与最终history记录新旧cut，旧业务已存在时保留前轮局部代次回归红证据，不把缺gate当产品缺陷。
- S6-20：补充产品场景虽通过但未归档成功状态/参数，不能据测试计数宣称矩阵证据完整。复用现有 PTY 公开 Session 采集与 finish_recorded，覆盖前轮精确列出的24项；先以归档覆盖检查观察缺包红，再增加采集。缺库拒绝只记真实错误、库不存在与 log/turn 不适用，不制造 Session。保留所有现行业务断言和预算；这是证据缺口，不称产品行为缺陷。
