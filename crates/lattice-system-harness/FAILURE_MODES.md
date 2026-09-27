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
