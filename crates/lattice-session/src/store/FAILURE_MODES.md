# 会话库失败方式与测试边界

本清单先于测试与实现编写。依据为 [会话合同](../../../../docs/architecture/session.md)、[存储合同](../../../../docs/architecture/storage.md)；是 S1 的测试设计，不另立规则。隔离 seam 为同步领域 API、纯关闭规划、版本编码及严格打开；损坏夹具允许直接修改临时库，断言仍通过领域读取/打开。所有目录均为测试自有临时目录。

下表的测试名是预定覆盖入口，只有实际执行通过才构成验证证据。组合测试中的各拒绝分支必须检查状态未变，不能只检查返回错误。

| 编号 | 失败前提与应保护的性质 | 隔离测试入口 |
| --- | --- | --- |
| F01 | 非规范 Session/UUID、复合键长度/前缀/整数排序错误；0 与整数最大值往返 | `keys_and_versions` |
| F02 | 值版本头截断、未知版本、非法 UTF-8/JSON/枚举、缺字段/多字段、错误表类型；包括冻结配置/limits/catalog/entry、消息/绑定/usage/错误等嵌套记录的未知字段；仅 Cell evidence 明确允许扩展，任意 JSON 载荷中的同名键不能被误认成 store 结构。错误不泄漏正文；参数/成功结果套入最终记录后超过同一解码条件时，提交前返回 InvalidInput 且全事务不变，可读值可读取、关闭及重开 | `keys_and_versions`, `strict_open_rejects_corruption`, `deep_arguments_are_rejected_before_commit`, `deep_results_are_rejected_before_commit`, `nested_unknown_fields_are_rejected`, `json_payload_field_names_are_not_store_records` |
| F03 | meta 缺项、错误 store_kind/格式版本；空文件、随机文件、目录、链接不能被 create 覆盖 | `creation_and_legacy`, `strict_open_rejects_corruption` |
| F04 | sessions.git 与任意项目 invocations.git（目录、文件、悬挂链接）必须全部列出；不得初始化或修改旧库 | `creation_and_legacy` |
| F05 | legacy 枚举/元数据错误不是不存在；projects 缺失合法；缺库只返回未初始化 | `creation_and_legacy` |
| F06 | 同名并发创建、NOREPLACE 失败、既有损坏赢家；不覆盖已发布目标 | `creation_failures` |
| F07 | 临时提交/发布前失败、发布后父目录同步失败、遗留清理失败；阶段分类正确，重试验证原目标 | `creation_failures` |
| F08 | 能力探测、文件锁、存储 I/O 失败不能降级；严格 open 不创建或修复表/索引 | `creation_failures`, `strict_open_rejects_corruption` |
| F09 | 根缺失/多根/非根 parent>=seq/父缺失/current 缺失；根 0 唯一且空历史合法 | `strict_open_rejects_corruption`, `tree_history_and_agents` |
| F10 | 接受输入、配置、目录、agent 来源、项目绑定无效；2MiB/目录1MiB边界，不产生部分接受 | `acceptance_identity_and_limits` |
| F11 | 全局 TurnId 找回须核输入/项目/指定 Session；New 重发不多建 Session，rewind 不释放 TurnId | `acceptance_identity_and_limits`, `tree_history_and_agents` |
| F12 | SessionId 碰撞；实际接受路径分配 NodeId 溢出、实际登记路径分配 ChangeSeq 溢出（含批内首项分配成功、次项耗尽）：返回 SequenceExhausted，不到达提交，全部表记录不变。极值夹具在已打开的临时库压缩序号空间，不冒充正常 API 生成的连续历史 | `acceptance_identity_and_limits`, `sequence_exhaustion_aborts_transactions` |
| F13 | 错 layout/Session 的锁、独立重入冲突；不同 Session 锁独立，读取不取操作锁 | `sequence_and_lock_boundaries` |
| F14 | active_turn 错指、孤立/多个 Running；接受后与 Continue 后无 Active 合法 | `strict_open_rejects_corruption`, `auto_close_matrix` |
| F15 | start_request 步骤/尝试不连续、重复 Requesting/Executing、32步骤/11尝试预算 | `attempts_and_step_budget` |
| F16 | stop_attempt 错 key/非法 cause/诊断；可重试间隙合法，其余停止同时关闭 Step/Turn，不入树 | `attempts_and_step_budget` |
| F17 | Requesting/RetryReady 只允许既有可重试且未耗尽的停止尝试；同一 ProviderError 可重试且预算未尽时不能直接 Closed；退避中的 Cancelled/Deadline/ProcessExit 不沿用旧诊断 | `auto_close_matrix`, `attempts_and_step_budget`, `impossible_transaction_shapes_are_rejected` |
| F18 | 登记无调用、非 Assistant、非对象参数、重复调用、超32、未知工具；拒绝整批 | `registration_and_ordering` |
| F19 | 候选批内重复/全局已占用；无部分槽位/索引/日志；新候选重试成功且旧记录不变 | `registration_and_ordering` |
| F20 | 登记不得重排文字/reasoning/调用/replay；绑定摘要/地址不符拒绝；Completed 不独立持久 | `registration_and_ordering`, `strict_open_rejects_corruption` |
| F21 | 乱序/重复 Started、前序停止终态、伪造 binding；不产生新的 Started | `registration_and_ordering`, `ordered_terminal_causes` |
| F22 | 非 Started/乱序/重复结束、错误 ID/name、Started→NotExecuted；终态/证据/索引/日志同事务 | `registration_and_ordering`, `strict_open_rejects_corruption` |
| F23 | 成功 JSON 规范字节65536/65537、Unicode/键顺序；大输出保留完整摘要长度且仍成功 | `payload_evidence_and_privacy` |
| F24 | 六种载荷形状与类型原因；无 receipt 不伪造证据、已有真实终态不覆盖 | `payload_evidence_and_privacy`, `auto_close_matrix` |
| F25 | evidence 三 root 版本/未知枚举、方法/序号不符拒绝；允许未知字段；失败仍保留 effect | `payload_evidence_and_privacy` |
| F26 | Finish 含调用、Continue 未完成/含停止、提前步骤上限；不产生未配对节点 | `registration_and_ordering`, `attempts_and_step_budget` |
| F27 | 第32步后不得第33次请求；无尝试行政 Stop 合法；关闭后写入拒绝 | `attempts_and_step_budget`, `auto_close_matrix` |
| F28 | 自动关闭表全部行：无Step、Requesting、RetryReady、非法Stopped、Registered、Started、全终态、Continue间隙、已Closed；每行独立断言精确 end/cause、每槽位载荷与证据、节点内容与尝试内容 | `auto_close_matrix` |
| F29 | 按片段首个停止事实而非严重性选择原因；确定失败/拒绝继续；孤立 PrecedingInvocationIndeterminate 拒绝 | `ordered_terminal_causes` |
| F30 | 规划纯且确定；保留既有终态、节点、usage；重复关闭不写时间/日志 | `auto_close_matrix` |
| F31 | Completed 无 notice，Cancelled Info，其余 Error；最终 provider 诊断来源、Unicode2048界限、缺诊断拒绝 | `attempts_and_step_budget`, `payload_evidence_and_privacy` |
| F32 | 无尝试、已知/未知混合、失败重试、饱和 usage 汇总；持久汇总须匹配重算 | `attempts_and_step_budget`, `strict_open_rejects_corruption` |
| F33 | rewind 根/旧支/同节点/不存在/active；只改位置，agent 派生，seq=max+1 | `tree_history_and_agents` |
| F34 | list 含根计数；log 全树与首节点结局；Turn 明细含无节点 Stop/尝试且拒绝跨 Session | `tree_history_and_agents` |
| F35 | snapshot active/current/root 选择；旧 Turn 仍按当前路径读；读事务不混合并发新状态 | `tree_history_and_agents` |
| F36 | 固定 head 跨页追加/rewind、分叉间隙、错 cursor/anchor、UTF-8边界、零/不足字符预算、大节点分片 | `tree_history_and_agents` |
| F37 | 索引缺/多/错地址/末变更，日志缺/非法转换/投影不等；严格打开与读取拒绝，不重建。tail 对每个匹配调用按有序生命周期校验，包括游标之前及被 outcome 过滤隐藏的前态；重复 Registered、缺失/重复 Started 均拒绝，交错的不同调用不混用前态 | `strict_open_rejects_corruption`, `tail_rejects_invalid_lifecycle_across_pages` |
| F38 | audit 项目隔离、生命周期/终态过滤、tail 排他/空页前进/高水位、replay顺序/零调用、status去重20 | `audit_snapshots_and_pagination` |
| F39 | operator/summary/detail 无参数/结果正文/schema/配置；错误与 Debug 不泄漏载荷 | `payload_evidence_and_privacy`, `audit_snapshots_and_pagination` |
| F40 | 两类 commit 错误可区分；有界 gate 确认其他 Session 写者已遭遇健康锁竞争，再放行提交；redb 写事务已释放、健康状态尚未更新的窗口内等待者不得完成，发布停写后全为 WriteDisabled；不自行重试 | `commit_failure_disables_all_writers`, `creation_failures` |
| F41 | accept/register/terminal/close 失败无部分关系，成功同一快照可见 | `commit_failure_disables_all_writers`, `registration_and_ordering`；真实逐提交点 SIGKILL 交 E2E |
| F42 | 读视图固定快照；库级停写不能仅作用于一个 Session；释放后重开按磁盘事实处理 | `commit_failure_disables_all_writers`, `tree_history_and_agents`；完整后台交接交 E2E |
| F43 | strict-open 仍检查全库；普通 session/turn/node/history/list 页及写事务只触达目标记录与直接关联，不解码其他 Session 或同 Session 的无关历史 Turn；目标关联损坏仍拒绝且不修复，候选 InvocationId 仍全局查重 | `ordinary_operations_are_local`, `local_reads_reject_broken_references`, `strict_open_rejects_corruption`, `registration_and_ordering`；不测延迟、不设阈值 |
| F44 | 打开后 meta 调用日志高水位低于或高于实际末键：登记前返回 Corruption，所有记录不变；发布新增日志时序号已占用也须拒绝，不能覆盖原行 | `registration_rejects_corrupt_change_high_water`, `change_publication_rejects_occupied_sequence` |

复核时细化的组合入口：
- `planner_rejects_inconsistent_durable_facts`：F14/F20/F28/F30，纯规划独立核验当前位置、用户节点、provider 与关闭槽位归属。
- `schema_and_legacy_rejection_matrix`：F02/F03/F04/F05/F08，错误表类型/缺表/版本、旧目录/文件/悬挂链接与数据库独占打开。
- `input_and_evidence_boundaries`：F10/F16/F22/F25/F31/F38，目录字节边界、receipt 身份、错误保留效果、重复效果序号、未知 evidence 枚举与查询过滤。
- `transaction_failure_matrix`：F40/F41/F42，开始请求、两类尝试停止、登记、开始/结束调用、Finish/Continue/Stop、自动关闭和 rewind 的提交前/后错误；接受由原提交故障测试覆盖。
- `impossible_transaction_shapes_are_rejected`：F17/F20/F22/F26/F28/F29，按 session.md 三组表核对下列互斥形状，纯 planner 与 strict-open 均不得通过自动关闭修复。

| 事务/不变量/自动关闭行 | 必须拒绝的不可能形状 | 验证入口 |
| --- | --- | --- |
| 开始请求、重试间隙 | attempt 不连续/越预算；既有 Completed、不可重试或耗尽 Stopped；空 RetryReady | `attempts_and_step_budget`, `impossible_transaction_shapes_are_rejected` |
| 登记调用、调用结束 | Executing 缺 Completed/空槽位；存在 NotExecuted 或无 receipt 的补写 Indeterminate（这些只能与关闭同事务） | `impossible_transaction_shapes_are_rejected` |
| 顺序执行、停止分发 | 终态不成前缀、多 Started、停止后执行；补写尾槽原因不一致；Indeterminate 后尾槽不是 PrecedingInvocationIndeterminate | `registration_and_ordering`, `ordered_terminal_causes`, `impossible_transaction_shapes_are_rejected` |
| 关闭 Step | Completed 与节点有无不符；Continue 无调用/有停止，Finish 有调用；Stop(Some) 无调用或 cause 不符；Stop(None) 非重试末尝试原因不符、重试预算未尽却沿用 ProviderError；无尝试却为尝试专属原因；提前 ModelStepLimit | `impossible_transaction_shapes_are_rejected`, `attempts_and_step_budget` |
| Turn 不变量 | Running 有 Finish/Stop；Closed 无 Step/有 Active/末尾 Continue；非最后记录不是 Continue；outcome/notice/usage 与最后 end 不符 | `strict_open_rejects_corruption`, `planner_rejects_inconsistent_durable_facts`, `impossible_transaction_shapes_are_rejected` |
| 自动关闭 | 非法上述中间态拒绝；合法无 Step、Requesting、RetryReady、Registered、Started、全终态、Continue 间隙、已 Closed 分别保留真实事实 | `auto_close_matrix` |


## E2E 承接与证据限制

- F41 的真实进程崩溃、F42 的 Owner/provider/Tool 全库禁启与关闭重开生命周期：`docs/development/testing.md`「后台崩溃与自动关闭」及「同会话排他与跨会话并发」。S1 只验证领域事务、错误分类和等待写者，不声称验证产品接线。
- F31/F39 的真实凭据脱敏：同文「并发配置与 TLS 隔离」及「观察快照、慢订阅与读取失败」。库接收已脱敏诊断，不能识别没有传给它的凭据。
- F35/F36 的内存观察锁/事件切点和 IPC 帧计量：同文「观察快照、慢订阅与读取失败」。S1 验证 redb 读快照与不可变节点分页，不实现订阅。
- F04/F05 的 CLI send/list/audit 拒绝接线：同文「旧库拒绝」。S1 的目录检测本身不推给 E2E。
- 故障注入只证明自有边界的控制与分类，不证明 redb 内部原子性或真实硬件掉电持久性；不降低真实文件后端与 Immediate 的要求。

## S2 / WP1 接缝（实施前清单）

沿用 S1 F01–F44；以下为 WP1 新增/迁移的隔离边界，不代表产品 E2E 已通过。

| 编号 | 失败方式与可证伪断言 | 入口 |
| --- | --- | --- |
| W1-01 | 结构化结果迁至 conversation 后改变 serde tag、Box、hex 或摘要；固定 S1 样本 round-trip 和摘要一致，非法内容仍由 store 拒绝 | store `structured_result_wire`、既有 `payload_evidence_and_privacy`、重开矩阵 |
| W1-01r | Rejected 丢失 CepError.message/data、旧无 message 形状误接受、错误码跨 outcome；保存→关闭→重开完整错误相等，operator 仍不含正文，嵌套未知字段拒绝 | `rejected_error_round_trip_and_privacy` |
| W1-09 | commit 释放数据库写者到禁写发布之间启动逃逸；同一健康锁内已交接者算在途，排队启动回调及所有后续启动均拒绝；回调 panic 后 fail closed | 扩展 `commit_failure_disables_all_writers`，`execution_start_panic_disables_store` |
| W1-05 | 观察安全投影缺失或从多个时刻拼接；同一 SessionRead 的节点 agent/槽位结果与 operator 状态保持固定快照 | 既有 F35/F38/F39，新增窄读取时扩展相应用例 |
| W1-06 | XDG guard 迁出后锁分裂或先解锁再恢复；设置及未设置原值均在 guard 释放前恢复 | `test_support` 子进程隔离检查；其他测试显式 StateLayout |

W1-02/W1-08 的失败清单分别放在 llm/agent crate；W1-03/04 放在 lancet-api；W1-07 放在 lancet-runtime。完整 Owner 停写/排空重开与公开 operator 跨项目 E2E 留 WP2–WP4。

## S2 / WP2 提交故障接缝

- 下游 Backend 必须能在真实写事务的 Poisoned、UnknownBefore、UnknownAfter 三个既有故障点核验错误传播、全库禁启与排空重开。仅在 `test-utils` 导出同步、线程局部的注入闭包，恢复上一个注入状态；不新增普通生产注入入口，不更改 Immediate 与禁写策略。覆盖入口为 API 的 `commit_fault_quiesces_other_session_until_reader_and_owner_really_retire`，S1 原故障矩阵继续复用相同 hit。

## S2 第二次审查：创建持久性未知

- Backend 的 CreationDurabilityUnknown 恢复必须重走既有 create 的严格核验与父目录同步分支；仅 open 不够。下游测试通过 test-utils 同步线程局部 probe 在真实父目录同步边界注入失败并计数，分别保护恢复同步成功后 Ready、再次失败仍不可用且目标 inode 不变。普通 open 不触发该 probe。

## S5 旧实现删除与纯配置迁移

- S5-01：迁移冻结配置时不能改变目录/配置摘要域、长度编码、字段 wire 或凭据键拒绝。保留并迁移原 config 的四个独立测试（目录边界、摘要自检、嵌套凭据、执行预算）；既有 F02/F10/重开覆盖持久读取。只迁移测试与错误类型，不增加旧符号不存在的断言。
- S5-02：删除 Git 会话夹具不能删除 API 工作区真实 Git 初始化能力；唯一仍在使用的非 bare fixture 迁至 API 的 dev support，原 workspace_history_acceptance / release_e2e 验证真实调用链。
- 旧 journal/CAS/候选修订/图拓扑测试随已批准删除的实现退出；新库原子性、关闭、绑定、快照与故障语义由 F01–F44 继续保护。红阶段与受控变异实际结果记录于本轮交接，不将既有测试迁移宣称为新功能 TDD。

## S6 外部故障产物

- S6-07：显式 test-crash-barriers 复用现有三个提交故障点的同步线程局部闭包，仅扩大该故障 seam 的 cfg，不启用 test-utils 的其他构造器。外部 harness 先观察缺故障红；普通构建无注入入口。原真实 redb/Immediate、健康锁与提交错误分类不变；不把注入当作硬件或 redb 内部故障资格化。
