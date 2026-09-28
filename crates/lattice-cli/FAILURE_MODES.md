# CLI R2 失败方式（WP3）

范围：计划 §6 W3-01/03/04/06/07/08；测试 seam 为 clap、命令渲染及真实 LocalClient/Unix socket。先登记，再迁移测试并观察红；依赖旧 TUI 导致的编译失败不冒称行为红。
S6 调度接缝：显式 crash 产物的 daemon 环境安装应保留 `LATTICE_TEST_SCHEDULE_DIR`，普通产物仍清除。外部 `slow_window_and_closed_snapshot_deliver_exact_history` 实测安装前屏障未命中；不为此新增普通产物隐藏行为或独立重复单测。


| ID | 失败方式与独立信号 |
| --- | --- |
| W3-01 | send/run 参数不一致；新提交不接受可选 SID/agent；NodeId 0/max 被拒绝或非十进制/溢出被接受；session --help 除 send/rewind/list/log/turn/cancel 外仍列额外命令。parser 与实际 help 精确核验同一 SendArgs、六命令和节点参数。 |
| W3-03 | 找回旧 Turn 强制加载配置；agent 未采用后台握手选定值。IPC fixture 在无凭据时完成 Existing，冻结只在 NeedsConfiguration 发生。 |
| W3-04 | 自行推导结局/notice，丢失 Failed Error/Cancelled Info。使用持久 TurnSummary 原对象。 |
| W3-06 | Completed 遮蔽最终正文失败、真实信号改写结局、迟到回执撤销已知完成。保留 run IPC 的取消、晚回执、读取/退出 0/1/2 风险。 |
| W3-07 | log 丢根或离路径分叉；turn 丢无节点/无尝试 Step；JSONL 混人类文本。固定树/TurnDetail 夹具及输出断言。 |
| W3-08 | audit/tail/replay/status 直接开库或加载 profile。真实 socket 请求断言及结构化读取错误；ChangeSeq 为排他数值游标。 |
| W3-06b | 后台已释放 Owner 的 Closed 与无 Owner 的 Running 混同，History 在 Finished 前到达时把成功误判为退出 1。真实 socket 使用 Closed+owned=false，Completed 应为 0，Failed 为 1，Cancelled 为 2；新空库的初始观察错误不覆写随后已确认的结果。 |
| W3-07b | JSON 观察流在 History 交付前执行人类 finish_text，产生裸空行；逐行解析原始 stdout，不跳过空行。 |

删除承接：旧 New/Agent/Status/Checkout/Branch/Diff/Reset 与文件恢复、project_branch/OID/checkpoint 专属解析/渲染/持久测试随批准退役，不保留墓碑扫描；会话身份、读取、全树/Step 输出由上述新契约测试承接。环境拒绝、doctor/model/路径与信号回归不退役。

本轮承接明细（原测试在旧 API 上的编译红见 `.pi/tmp/wp3-round1/entry-clippy.log`）：
- main/session 中旧 New/Agent/Status/branch/checkout/reset/diff、project_branch 持久化和文件 Rewind partial 专属测试按批准退役；六命令、NodeId、取消身份/帮助与全部树输出由 session_surface_and_numeric_nodes、r2_send_run_and_node_surface、tree_output_keeps_root_branches_current_and_notice 承接。旧 summarize/display_id 的 ASCII/CJK 边界合入 summaries_and_ids_respect_character_boundaries。
- run_tests 的 token/stdout、reasoning、字节数、错误摘要、中途换行、终结文案、JSONL 和 verbose 输出迁至相同渲染 seam；usage 不再累加观察事件，以持久 UsageSummary（含未知尝试）显示。旧 commit/Interrupted/checkpoint 事件序列退出；执行错误由结构化 LocalError 保护。
- 原 run_integration_commits_existing_session 实际只调用 API::run_turn、不进入 CLI；其返回与保存正文、usage 风险已由 API run_tests::run_turn_result_and_saved_nodes_preserve_contract_fields 和 next_turn_uses_root_path_parent_and_frozen_model_provider_prompt 承接，本轮不新增另一个 CLI 内进程内产品测试。
- run_ipc_tests 保留真实 SIGINT、两种取消回执顺序、取消后 detach 原身份、晚 NeedsConfiguration、未知后本地传输错误、NoOwner 有界退出、读错误/挂起与成功重载风险；旧完成确认等待改为 Closed summary 先交、完整 History 后 Finished。旧 query 的 selected transcript 正文语义退出，turn 详情现在只读全部 Step；跟随模式继续测试最终正文缺失退出1。
- resolve.rs 仅补新增 LocalError.store=None，是本轮发现的必要编译接缝；没有修改错误来源或环境策略。

- W4-CLI-01：无 SID、带原 TurnId 的找回在 clap 阶段被强制要求 agent，无法进入全局身份查询。WP4 真实未读回执/后台重启场景已复现 exit2，provider/库无需变更；新增 parser 回归先红，再移除前端 agent 强制，保留后台仅对新接受/根节点执行 agent 必填检查。
