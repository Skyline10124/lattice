# S2 / WP2 隔离失败方式

依据：已批准会话、存储、LANCET 合同与 `.pi/tmp/r2-s2-plan.md` §2–§5。此清单先于本包新增测试与实现；不是通过报告。真实二进制场景归 WP3/WP4，不以局部夹具冒充产品 E2E。

| ID | 前提与独立失败信号 | 验证接缝 |
| --- | --- | --- |
| W2-01 | 无 SID 同 TurnId 并发、接受回执丢失：唯一 Session/Turn；原 input/project/SID 不同 Conflict，找回不加载配置 | Backend 准入、LocalClient 惰性握手 |
| W2-01f | Owner 已返回接受且仍在执行时，同 TurnId 找回应从持久 Turn 返回 Existing，不重复宣称新 Accepted；接受尚未确定的并发交付仍共享原回执 | WP4 S2-VERTICAL 原实现实际返回 accepted（vertical-02.log）；公开 IPC 重发保护，不将此 E2E 红倒填为新增隔离测试红 |
| W2-02 | Owner 在准备、接受后、Continue 间隙、收尾：同 SID 新请求/rewind Busy；另一 Session 能请求模型 | Backend/执行 gate |
| W2-03 | 根无 agent、继承 agent 改变、二次提交配置不匹配：接受前拒绝，无新 User；已存在 Turn 忽略新配置 | 配置握手、接受 |
| W2-04 | typed retryable、usage=None、退避取消/Deadline、32 步限制：持久原因和计数准确，不启动第 33 次模型请求 | Coordinator + 临时文件库 |
| W2-05 | InvocationId 冲突、Started/Terminal 失败：仅重试登记、不重试 provider；失败后不分发后槽 | Coordinator + store/启动 gate |
| W2-06 | 工作区锁占用或 history 损坏：closure-only 只依赖会话事实，不打开 history、不组成 runtime | 自动关闭/rewind |
| W2-07 | 两类 commit 错误、worker panic：全库禁启；实际旧代读写/Owner 排空后单次重开；无自等待、无内存 receipt 跨代重放 | Backend 管理槽、启动/排空 gate |
| W2-08 | 无 Owner 的 Running：query/subscribe/audit/cancel 不关闭、不加载配置、不建库；新 send/rewind 才关闭 | Backend 只读入口 |
| W2-09 | 超过 64 事件、重试、Executing：同锁更新 seq/段；窗口落后重同步，段按 attempt 精确替代 | Owner 观察锁/订阅 |
| W2-10 | close commit 后事件前、慢/失败历史、旧读取迟到：先交 summary/steps；实际正文完整前不发 Finished；旧成功不覆盖新代，旧失败保留 stale | Snapshot/LocalTurn |
| W2-11 | v2、零长/超限帧、合法大记录、UTF-8/JSON 转义、错 ID/offset、未完成 EOF：拒绝非法帧；只交完整对象，逻辑响应顺序与每帧上限不变 | `local/ipc.rs` ResponseFrame 读写 |
| W2-12 | 首次无 SID 取消、两种回执顺序、Taken 后退出、未知提交迟到：同目标最多一次补发，控制不等正文，Drop 不新增 cancel | LocalTurn/Backend cancel |
| W2-13 | 缺库/多个 legacy/损坏版本：结构化安全错误，不泄露配置/参数/诊断 partial，不初始化 | StoreError → LocalError/IPC |
| W2-14 | Owner panic、事件消费者退出、stop：独立 completion 排空后才移除 Owner，等待期间 query/cancel 可用 | 执行监督/Backend stop |

## 红阶段记录纪律

每个闭环记录原实现上的行为红；接口尚缺失或整个 API 因 WP1 硬切尚不能编译时，只称接口/编译红。必要的独立编译驱动和受控变异另记适用范围，不能把其他模块编译错误算作目标断言失败。旧测试的有效风险先承接再删除；不删除历史证据。

## WP2 第 1 轮：单库协调接缝

- W2-05a：通过真实临时文件库接受→Requesting→登记两个调用；启动交接回调读取到 Started 且恰好一次，重复 Started/错误 binding 不启动回调；不得用 InvocationBinding 代替 StartedInvocation。
- W2-06a：无工作区历史库（目标路径故意为损坏普通文件）、项目独占操作锁已持有，Coordinator 自动关闭仍完成；rewind 只移动 current，Turn/节点与该文件字节不变。
- W2-05b：InvocationId 与前一 Turn 冲突，仅重试登记整批候选；模型回复不重取，已提交调用身份不替换。
- W2-07a：健康锁已停写后，Coordinator 的统一执行交接拒绝 provider/Tool 的预构造 future；启动计数为零。Backend 的跨 Owner 排空/重开仍须另测，不从本项外推。
- 原 coordinator 内测承接：真实双库读取/预留投影由单库接受与槽位读取测试替代；错误项目调用库已无第二库对象，项目绑定拒绝由 SessionStore 接受事务与 API 准入保护；checkpoint 锁测试随已批准功能删除，以 W2-06a 的关闭隔离反向要求替代；随机/确定 ID 检查保留。
- W2-09a：同步入口在无人消费通知时连续接收 70 个正文/推理片段，快照段完整而事件窗口只有最后 64 条；过期、未来游标均要求重新快照，不能继续残缺事件。
- W2-09b：新尝试清旧段，旧 key delta 不推进 seq；终结事件后迟到 CEP 不再追加；同锁 begin_read 取得的持久 Step 状态决定是否保留段（Executing 保留、RetryReady/Closed 替代）。
- 原 observer 的“丢失仍编号”测试被新无损段+窗口重同步测试取代。CEP 延迟 finished 的有效显示顺序由执行端真实排空事件后才发持久 ToolCallFinished/Turn 终结保护，不再保留二次有损转发器。

## WP2 第 2 轮：live 事务循环

- W2-04a/W2-05c：真实临时 redb 与 Component 运行时，模型先给两个调用（成功与确定拒绝），再给零调用回复；仅两次 provider 请求，两个槽位先 Started 后终态，两个 Step 为 Continue/Finish，最终事件与持久 summary/Step 精确一致。输入历史来自 User/Step 节点，冻结 provider/model/system_prompt 保持原值。
- W2-04b：流中已产生 delta 后 retryable 错误仍重试；旧尝试保留未知 usage，下一尝试为空段开始，终结通知在持久关闭之后；不可重试错误不启动下一请求。
- W2-04c：在 RetryReady 的退避期间取消，保存无节点 Cancelled Stop，不沿用旧 ProviderError；执行停止标志与用户取消分开，库停机不写用户 Cancelled。
- W2-04d：沿用旧 stream 退避独立风险：40/60/60ms 精确调度，退避取消不发第二请求，显式 1s / 默认 300s Deadline 在退避中生效；通知的 delay_ms 也须受剩余 Turn 预算约束。迁移后的原断言实测发现通知 60000ms、实际预算 1000ms，不改期望值。
- W2-14b：显式 test-crash-barriers 产物在接受、Requesting、RetryReady、Registered、Started（runtime 接管前）、Terminal、StepClosed（通知前）落盘后提供 marker；包含 Session/Turn/Step/Invocation/ChangeSeq，不含正文、配置、Git OID 或凭据。选择器可精确选 Step/part，未命中不暂停；SIGSTOP 的真实进程验证留外部 harness，不以纯选择器测试冒称崩溃 E2E。
- W2-05d：已登记完整混合批次含未知工具时整批 InvalidBatch，执行计数为零；成功超大内容仍是 Succeeded(OutputTooLarge)，不退回字符串结果。
- W2-14a：已交接模型/Tool worker 必须真实 join 后才释放操作锁；在途 future 不因观察退出或协调错误被 abort，库故障仅请求协作停止，receipt 不跨代重放。
- 旧 saga 测试迁移对照：错误 outcome/notice 由新 live 不可重试、取消与 session close 的持久测试承接；unknown/mixed batch 由 W2-05d 承接；超大结果由 W2-05d 与 session/llm 原有结果测试承接；错误文字截断归 llm projection；旧 finalize budget、ledger terminal-before-journal、候选修订是已批准退役协议，删除时不保留墓碑测试。
- W2-13b：SessionApi 只包装传入的当前代 SessionStore；会话解析拒绝空/过短、精确优先、跨页唯一前缀解析，歧义最多五项。原 session.rs 的 graph-only 替换与隐式 XDG 风险由无 open/create 的传入句柄接口承接；list/count/log 风险由新 SessionApi 读取与 S1 list/log 测试承接；branch/checkout/reset/diff/project_branch 专属测试随明确删除的产品契约退役。

## WP2 续做第 1 轮：当前代租约与真实排空

- W2-07b：两条 SessionApi 租约共享一个库代；首个 CommitFailed/WriteDisabled 将整代转为 Quiescing 并置独立 shutdown（不改用户 cancel）。此后 acquire/启动均拒绝；旧租约仍持有时恢复不得完成。
- W2-07c：阻塞读取已开始后丢弃其 async 调用者，读取闭包仍持有旧代租约；必须等 gate 释放、真实阻塞 worker 返回，才能关闭旧库并严格重开。不能以 abort 返回或 Arc 计数轮询证明排空。
- W2-07d：重开只读，不 auto_close、不重放 receipt、不创建缺库；单次失败只安排一次恢复。重开失败保留 Unavailable，不循环 create。
- W2-13c：只读 acquire 遇到缺库不生成 lattice.redb；只有显式新 Session 接受路径允许 create。管理槽打开与 legacy 错误不当成缺库。
- W2-07e：调用者已放弃等待后阻塞 worker panic，仍须在旧租约释放前触发库代恢复；正常任务调用者不得成为唯一错误上报者。

## WP2 当前续做：Backend 与 v3 读取闭环

- W2-01/03/08/13d：真实私有 socket 上无 SID probe 必须返回所选 agent 而不建库；新 Session 缺 agent 在任何建库/工作区历史之前拒绝。缺库 list/query/cancel 保留 NotInitialized，多个 legacy 路径完整返回。
- W2-09/10b：服务端 Closed 快照先交完整 summary/Step；随后按固定 anchor 读取历史，Finished 携带完整树路径。无 Owner Running 仅快照不发 Finished；读取失败单独 HistoryError，不伪造完成。
- W2-11b：所有后台响应（含错误与订阅）和 LocalClient 都使用连接级 ResponseWriter/Reader；保留部分帧取消安全，逐片进展刷新 idle。
- 入口实测 cargo check -p lattice-api --all-targets 为 281 errors（非续做提示中的106）；目前并未执行到行为断言，不能作为新断言的行为红。
- W2-01e：同 Session 正在执行新 Turn 时，找回该 Session 的已持久旧 Turn 必须先核验全局 TurnId，再返回 Existing；不能在 Session Busy 判断中吞掉只读找回。以公开 IPC + 被 gate 挂起的真实 provider 请求复现。

## WP2 第 2/6 轮续做：既有 API 验收迁移

- 原 usage_turn 三项风险保持：真实 mutate.edit 成功且持久槽位成功；第二 Turn 使用第一 Turn 节点上下文并形成正确父关系；目录外调用拒绝仍保存已报告 usage。改用接受时创建 Session、Node/Step/TurnDetail，不保留目标分支和 Git commit 断言。
- 原 release_e2e 的公开事件与结果一致性、跨存储正常文件历史关联继续保留；文件恢复专属场景仅在逐项确认不再承担现行风险后退役。
- 本次入口 all-targets 编译实际失败：client_tests/server_cancel_tests 等 lib test 共605错误，另有 release_e2e 21、usage_turn 22、workspace_history_acceptance 31、rewind_acceptance 123、prefix 1；生产 --lib clippy 已通过。这些是继承接口迁移编译红，不冒称行为红。
- workspace_history_acceptance：保留运行时安装前拒绝、安装后不回滚、顺序/前态/用户 Git 不变、可达性与封闭目录；删除只属于文件 rewind 的回退 current/retention pin 子断言。Turn 层三项改按历史 commit 的真实 TurnId/InvocationId 与重开会话槽位核对；无变更不移动 history、失败 Turn 不撤销已安装变更继续断言。测试显式 StateLayout，不再更改 cwd 或保留第二把 XDG 锁。
- 原运行时 helper 的无绑定未知方法调用已无法通过 Started 凭据构造；未知目录/混合批次在登记前拒绝由 usage_turn::usage_rejected_out_of_catalog_tool_batch_keeps_the_attempt_usage、turn::live_tests 对应整批拒绝承接，不伪造 Started 或在测试 helper 合成 receipt。history_adds_no_agent_visible_method 保留完整目录精确断言；安装后失败保留两个真实前态拒绝。迁移中原 !tool.ok 误写为 Failed，实测暴露 Guard/ReplanRequired 应为 Rejected，按 prepare_terminal 的 Guard 分类修正为精确 Rejected，不改变原失败标准。
- 退役测试承接：git_store_branch_checkout、git_store_diff、git_store_reset 只测已删除的 Git 分支/差异/reset/OID；按计划删除。移动当前位置但保留原 Turn/节点的现行风险由 workspace_history_acceptance::turn_history_bindings_survive_session_rewind_and_reopen 及 git_store_session_create_open::readonly_finished_delivers_full_history_and_rewind_only_moves_current 承接。git_store_session_prefix_resolve 的精确/4字符/过短三项由 session::redb_tests::passed_store_prefix_resolution_and_safe_metadata 原断言承接（另含空白/歧义）；该 lib 测试仍受其他旧测试编译阻塞，未冒称已运行。
- profile_snapshot 的独立风险迁入首次接受重开测试：接受后改磁盘 profile，重开仍保存原 model/system_prompt 与完整 profile JSON。head_transcript 的消息精确顺序迁入 usage_resume_session 的实际后续 provider 请求与第三 Turn 节点读取；turn_commit_history 的持久/可达风险由首次接受关闭重开与前述 Node 读取承接，Git OID/tree/ref 表示断言退役。concurrent_commit_safety 的 Git CAS 场景暂保留，待同 Session 竞争验收可运行后再删除。
- 删除 rewind_acceptance.rs（计划明确整文件删除）：恢复范围/同路径冲突/逆序安装/回退 history current/双引用分裂/恢复审计重试与屏障均专属已退役文件恢复。正常安装前态、用户 .git 不变、只追加可达性保留于 workspace_history_acceptance；会话只移 current 且历史/文件不变由上述新语义测试保护。不将原恢复审计断言改名保留成第二恢复入口。
- W2-14b 显式 feature 编译红：test-crash-barriers 的 marker 错用 InvocationBinding.step/part_ordinal 与不存在的 SessionRead.change_seq，4 个实际编译错误。修正只读取既有 identity 地址及同一 SessionRead 的 operator_status.latest_change；不新开库、不新增持久字段或改屏障语义。selector 原测试已存在，但 lib 旧测试尚阻塞其执行。
- W2-02 的既有并发防覆盖风险承接到公开 IPC gate：同一 Session 的四个不同 TurnId 同时提交，provider 未释放时恰好一个 Accepted、三个 Busy；Busy 请求无 Turn，rewind 同样 Busy；同时找回旧 Turn 仍 Existing。gate 释放后再检查只有获胜者新增 User/Step，不靠耗时判定并发。
- 上述四请求竞争已实际 1 passed（concurrent-green2.log），取代 git_store_concurrent_commit_safety 的两项 Git CAS tests，删除旧文件；同父冲突的旧 CAS 表示不继续维护，现行保护对象为全 Turn 锁下单次接受与无覆盖。新增请求竞争断言在继承实现上直接通过，不冒称原行为红；先前旧测试本身依赖已退役 Git fixture。
- local/tests.rs 迁移保留：socket 私权/第二 Owner/超长帧，两个 Session provider 同时在途，同 Turn 共享/Conflict、同 Session Busy、stop 排空期间 query/cancel、重启只读/根路径分页不变；probe/rewind 在无配置时关闭遗留、新配置 loader 最多一次且只请求新 Turn。旧 branch/diff/SetAgent/完成确认专属断言删除，Gap 改为快照重同步。该文件初始69个编译错误先记录于 all-targets-clippy.log。

## WP2 第 3/6 轮：剩余本地协议回归

- 入口 check 实际仍为536个 lib test 编译错误，不是提示中的106；保留继承实现，以新领域 DTO 重写旧协议夹具。
- client_tests 的独立风险全部承接：固定根路径及分片/agent归属；summary先于慢正文、完整History先于Finished、错误/idle不撤销summary；取消两种回执次序和一次补发、旧意图回执隔离、具体错误保留/清除、Taken后detach、五秒未知与迟到传输/后台/本地配置错误区分；惰性借用loader、放弃冻结、接受后换观察切点；背压主动重订阅及旧正文成功/失败退休。旧 merge/selected-pin/CompletionConfirmation/Gap/TextAction 的表示断言按已批准退役，保留对应当前风险而非旧表示。
- Backend 旧补屏障确认测试按Closed可读即持久退役；保留关闭事件尚未追加时的即时summary与完整历史、快照/Owner交接、读取失败及实际completion排空风险。修改前不宣称这些新夹具已行为通过。

## WP2 第 5/6 轮：监督、观察及取消迁移

- 入口实际为242个 lib test 编译错误：继承 client_tests 重写已基本完成，仅1处 TokenUsage 构造错误；server 内测173、server_cancel_tests 68仍用旧接口。继续保留前轮成果，不把编译失败当行为红。
- Backend 内测迁移：删除 SetAgent 独有能力（命令已退役）；补屏障/缓存确认专属断言由 Closed 快照无需等待 Owner 即交 summary/Step/完整历史承接。保留接受 continuation 前观察、快照期间 Owner 更换重读、>64窗口和当前段、关闭后读取失败、重开及其他 Turn 锁不阻塞历史读取、冲突不覆盖原诊断。
- 取消内测迁移：保持准备、无配置 probe、共享 probe 后竞争、拒绝后重试及旧在途配置、同锁取消/退休、stop 和 panic 独立 completion 的各个 gate。只改测试调度入口，不增加普通产物行为。
- commit 故障测试须从真实 API blocking 写操作的错误触发管理槽；两类注入分别验证旧代的其他执行/读租约排空、启动被拒绝及单次严格重开。旧 object/ref-fsync 故障与 ledger OID 断言不保留，改为会话事务 before/after commit 的事实核验。
- 本轮续做补充 W2-07f：真实第二 Session 的 provider 在途时，让第一 Session 的 auto_close 事务分别在回滚、提交前未知、提交后未知三个边界失败。跨 Session 必须停新启动并排空 provider；阻塞读 gate 未释放前不能重开，释放后严格重开一次，读取磁盘事实，不自动关闭第二 Turn、不把库故障记为用户 Cancelled。下游注入只复用 session 现有 commit 故障 seam，限定 test-utils。

## WP2 第 6/6 轮：实际故障回归闭环

- 继承代码已通过 all-targets clippy；首次全 lib 运行88通过/9个取消测试超时，同输入复跑97通过，保留首次失败，不通过放宽10秒门限或串行测试掩盖。
- W2-07f 继续落地：新增跨 Session Backend 回归，调用真实 auto_close 事务并复用存储层提交故障点；先记录缺失测试 seam 的编译红，再以禁用错误上报的受控变异证明能检测未全库停写。
- 使用阻塞读与实际 provider worker 的独立 gate：两者任一未退场时不能重开；重开后两个 Turn 的事实分别对应注入点，第二 Turn 保持 Running/Requesting，无新的 provider/Tool 启动。
- W2-11c：NoOwner Running 的历史读取仍可持续取得 ResponseFrame 分片；累计读取超过300秒但每片间隔不足idle时不得超时。用暂停时钟的实际socket分片先复现，随后将历史页的分片进度接入 LocalTurn，不改变idle阈值。
- 退役承接补齐：继承 diff 已从 turn/mod.rs 移除 collector/mapping 的模块与导出，本轮删除孤立文件。旧 collector 的持久终态传播由 run_tests::run_turn_result_and_saved_nodes_preserve_contract_fields 与 types_tests::terminal_events_round_trip_the_persistent_summary_and_step 承接；旧 Git 补屏障和 invocation reconciliation 错误专属测试退出，当前提交错误传播由跨 Session commit 故障回归承接。mapping 的冻结 model/provider/prompt 已归 live::freeze_config/execute_owned，现有 run_tests::next_turn_uses_root_path_parent_and_frozen_model_provider_prompt 保护；不保留 cwd 回退。

## 第二次审查修正 F10–F12

- F10：无 SID 首发已持久接受、接受 continuation 暂停时，公开 query 已可找到 SID；竞争 Submit/rewind 必须 Busy，原 Owner 仍收到 Accepted、仅执行一次。使用 Backend 真实提交、accepted/probe gate 和临时 redb，不覆盖另一 Owner 的准入索引。
- F11：首次创建 rename 已发布但父目录同步失败，管理槽恢复必须保留创建未知原因，排空后严格核验原目标并重做父目录同步；成功才 Ready，重试同步失败仍不可用且不删除目标。使用仓库 test-utils 的父目录同步故障 seam，经真实 acquire(true) 触发，记录同步尝试；普通 commit 故障仍严格 open。
- F12：Requesting/Started 写入或健康锁交接等待跨过 deadline/取消时，不再启动 provider/Tool。协调器最终 handoff gate 与模型首次打开前分别核验；Requesting 写 Stopped(Deadline/Cancelled)，Started 无 receipt 按合同补 Indeterminate，尾槽和 Step/Turn 同事务关闭，必要写入不受 deadline 限制。使用真实临时库和受控交接 gate，不从最终失败结局反推没有执行。
- 本轮先登记、再新增回归：F10 在接受回执 gate 暂停期间查询 SID，用竞争 probe 的到达或 Busy 回执二选一同步，随后释放原回执；同时核原请求 Accepted、竞争请求 Busy、一次 provider 请求及持久关闭，避免修复后仍等待不应进入的 probe gate。
