# 全量旧测试 Triage 清单（issue #22 / 测试重构 3/6）

> **性质**：判决书与人工闸口。**本文件不删除、不修改任何测试代码。**
> 供 4/6 分批执行时直接引用。
>
> **判决标准**：RFC #19 决定二（四层）、决定六（旧测试不保证迁移）；父 PRD #18。
>
> **枚举快照**：2026-07-09，workspace 内 `crates/**` + `xtask/**` 的 `#[test]` / `#[tokio::test]` 共 **1449** 条。

## 1. 如何读这份清单

| 字段 | 含义 |
|---|---|
| 标识 | `crate` + 测试函数名 + 源文件:行号 |
| 决定 | **保留** / **提升** / **删除** |
| 目标层 | 局部不变量 / 快速二进制黑盒 / Direct-Local 外部二进制（#40 后；原「真实 Microsandbox」层已退役） |
| 替代场景 | 提升或「删除且行为已被覆盖」时指向的已落地或 planned 场景；弱断言/源码扫描删除写 `n/a` |
| 理由 | 一句话判决依据 |

### 判决口径（复述 RFC #19 决定六）

- 保护**局部不变量** → **保留**
- 保护**产品行为**但从内部测 → **提升**到二进制/Direct-Local 场景
- 保护**已退役结构**、只断言 `Ok`/可构造、扫描源码 import → **删除**
- 同一 happy path 已被更高层覆盖 → 删低层重复（4/6 执行；本清单标「提升」或注明替代）
- **不设**迁移率、数量或覆盖率目标

### 场景目录（替代场景 ID）

> Active 场景 ID 登记在此表。`msb_full_turn` / `planned_*_msb` 是 #40
> 退役的 MSB/KVM 词汇，仅在下方「历史/obsolete」段以 `~~删除线~~` 标注保留，
> 不再出现在 machine ledger 的 `replacement` / `target_layer` 或本目录的 active 行。

| ID | 说明 | 状态 |
|---|---|---|
| `fast_cli_help` | fast-binary: CLI help 表面 | 已落地 |
| `pty_tui_smoke` | fast-binary: TUI 入口 smoke（#40 起 Direct-Local ordinary lane，无 KVM/MSB 依赖） | 已落地 |
| `planned_session_cli` | fast-binary: `session new/list/status/log/branch --json`（no-turn 子集） | **已落地** `tests/fast_session_cli.rs`（#23 切片 1/2） |
| `planned_audit_cli` | fast-binary: `audit query/replay/tail` 缺失 store 错误态 `--json` | **已落地（部分）** `tests/fast_audit_cli.rs`（#23 批次 2；seeded 态归 `planned_direct_audit_*`） |
| `planned_config_cli` | fast-binary: `models`/`resolve`/`doctor` + secret redaction + HOME/XDG | **已落地** `tests/fast_config_cli.rs`（#23 批次 1） |
| `planned_lancet_cli` | fast-binary: `lattice lancet status/doctor` 空态/不变量 | **已落地** `tests/fast_lancet_cli.rs`（#23 切片 1，空态；seeded 态归 `planned_direct_lancet_status_seeded`） |
| `direct_local_turn` | Direct-Local: plain-text turn / session reopen / policy denial | **已落地（骨架）** `tests/direct_local_turn.rs`（#40；完整矩阵归 #24） |
| `direct_local_plain_text_turn_commits_and_exit_zero` | Direct-Local: plain-text 完整 turn + exit 0 + session log commit | **已落地** `tests/direct_local_turn.rs:108`（#40；接管旧 `msb_full_turn` 语义核心） |
| `planned_direct_message_assembly` | Direct-Local: AgentLoop 从 transcript + user input 组装消息顺序/内容 | planned（#24 assertion-level） |
| `planned_direct_tool_error_payload` | Direct-Local: CEP tool error 结构化 payload 保留 | planned（#24 assertion-level） |
| `planned_direct_streaming_chat_compat` | Direct-Local: Fake LLM chat-only streaming 向后兼容 | planned（#24 assertion-level） |
| `planned_direct_basic_completed_event_contract` | Direct-Local: TurnStarted/TurnCommitting 事件序 + started/committed session ID 相等 + 无 failed/cancelled | planned（#24 assertion-level） |
| `planned_direct_live_error_commit` | Direct-Local: live turn error 提交 error commit | planned（#24 assertion-level） |
| `planned_direct_live_event_order` | Direct-Local: live turn 事件顺序 | planned（#24 assertion-level） |
| `planned_direct_provider_startup_failure` | Direct-Local: provider 启动失败不产生 stream | planned（#24 assertion-level） |
| `planned_direct_delta_only_commit` | Direct-Local: commit 只存储 delta | planned（#24 assertion-level） |
| `planned_direct_branch_population` | Direct-Local: branch 在 turn 后被填充 | planned（#24 assertion-level） |
| `planned_direct_commit_failure` | Direct-Local: commit 失败确定性（取代旧 planned_resume_msb） | planned（#24 assertion-level） |
| `planned_direct_receiver_drop_commit` | Direct-Local: receiver drop 不取消 commit | planned（#24 assertion-level） |
| `planned_direct_slow_observer_commit` | Direct-Local: 慢 observer 仍 commit | planned（#24 assertion-level） |
| `planned_direct_tool_activity_before_commit` | Direct-Local: tool activity 在 committed 前发出 | planned（#24 assertion-level） |
| `planned_direct_stream_error_commit` | Direct-Local: stream error 仍 commit error | planned（#24 assertion-level） |
| `planned_direct_cancelled_commit` | Direct-Local: cancelled commit 提交 cancelled | planned（#24 assertion-level） |
| `planned_direct_partial_output_error_commit` | Direct-Local: partial output 后 error 仍 commit error | planned（#24 assertion-level） |
| `planned_direct_cep_lifecycle` | Direct-Local: CEP lifecycle 事件 | planned（#24 assertion-level） |
| `planned_direct_turn_audit_replay` | Direct-Local: 完整 turn audit rows + replay fragment | planned（#24 assertion-level） |
| `planned_direct_partial_token_parity` | Direct-Local: partial token 一致性 | planned（#24 assertion-level） |
| `planned_direct_tool_call_parity` | Direct-Local: tool call path 一致性 | planned（#24 assertion-level） |
| `planned_direct_activity_commit_order` | Direct-Local: activity 在 commit 前发出 | planned（#24 assertion-level） |
| `planned_direct_completed_disposition_parity` | Direct-Local: completed disposition 一致性 | planned（#24 assertion-level） |
| `planned_direct_error_disposition_parity` | Direct-Local: error disposition 一致性 | planned（#24 assertion-level） |
| `planned_direct_workspace_read_turn` | Direct-Local: default profile run_turn 读取 workspace 文件 | planned（#24 assertion-level） |
| `planned_direct_routed_provider_event` | Direct-Local: model_started 含 routed provider | planned（#24 assertion-level） |
| `planned_direct_reopen_previous_context` | Direct-Local: resume session 含 previous turn context（取代旧 planned_resume_msb 的 reopen 半） | planned（#24 assertion-level） |
| `planned_direct_fetch_capability_denial` | Direct-Local: https public fetch 需 fetch capability，拒绝缺 capability | planned（#24 assertion-level） |
| `planned_direct_disabled_fetch_denial` | Direct-Local: profile 禁用 web fetch 时拒绝 | planned（#24 assertion-level） |
| `planned_direct_audit_query_seeded` | Direct-Local: audit query seeded DB fragment | planned（#24 assertion-level） |
| `planned_direct_audit_tail_seeded` | Direct-Local: audit tail seeded DB rows | planned（#24 assertion-level） |
| `planned_direct_audit_query_unknown_session` | Direct-Local: audit query unknown session 空结果 | planned（#24 assertion-level） |
| `planned_direct_audit_replay_trace` | Direct-Local: audit replay 重构 turn trace（seeded DB） | planned（#24 assertion-level） |
| `planned_direct_audit_attestation_match` | Direct-Local: audit replay 报告 stored attestation 匹配 | planned（#24 assertion-level） |
| `planned_direct_audit_replay_unknown_turn` | Direct-Local: audit replay unknown turn 报错 | planned（#24 assertion-level） |
| `planned_direct_lancet_status_seeded` | Direct-Local: lancet status seeded audit JSON | planned（#24 assertion-level） |
| `planned_direct_provider_timeout` | Direct-Local: provider stream timeout 触发 | planned（#24 assertion-level） |
| `planned_direct_existing_session_run_result` | Direct-Local: run_integration commits existing session 的 RunTurnResult.assistant_output/status | planned（#24 assertion-level） |
| `planned_direct_canonical_workspace_root` | Direct-Local: tool executor 使用 workspace root 而非 profile package dir | planned（#24 assertion-level） |
| `planned_direct_branch_listing_isolation` | Direct-Local: branch listing 只读 addressed session refs | planned（#24 assertion-level） |
| `planned_direct_checkout_isolation` | Direct-Local: checkout 只改 addressed session HEAD | planned（#24 assertion-level） |
| `planned_direct_branch_create_isolation` | Direct-Local: branch create 只影响 addressed session | planned（#24 assertion-level） |
| `planned_direct_reset_isolation` | Direct-Local: reset 只移动 addressed session branch pointer | planned（#24 assertion-level） |
| `planned_direct_diff_isolation` | Direct-Local: diff refs 在 addressed session 内解析 | planned（#24 assertion-level） |
| `planned_direct_session_log_turn` | Direct-Local: session log 显示 seeded turn | planned（#24 assertion-level） |

#### 历史/obsolete 场景 ID（#40 退役；仅供追溯，不在 active ledger 使用）

| ID | 说明 | 状态 |
|---|---|---|
| `msb_full_turn` | ~~已落地~~ **#40 已删**：MSB/KVM 场景退役；语义核心由 `direct_local_plain_text_turn_commits_and_exit_zero` 接管 | 已退役 |
| `planned_observe_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_workspace_read_turn` / `planned_direct_canonical_workspace_root` / `planned_direct_tool_activity_before_commit` 等 assertion-level #24 ID | 已退役 |
| `planned_mutate_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_turn_audit_replay` 等 assertion-level #24 ID | 已退役 |
| `planned_retired_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_cep_lifecycle` 等 assertion-level #24 ID | 已退役 |
| `planned_policy_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_fetch_capability_denial` / `planned_direct_disabled_fetch_denial` / `planned_direct_cep_lifecycle` 等 assertion-level #24 ID | 已退役 |
| `planned_provider_fail_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_provider_startup_failure` / `planned_direct_provider_timeout` / `planned_direct_stream_error_commit` 等 assertion-level #24 ID | 已退役 |
| `planned_resume_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_commit_failure` / `planned_direct_reopen_previous_context` assertion-level #24 ID（#40 已落地 reopen 骨架 `direct_local_session_reopen_after_external_change`） | 已退役 |
| `planned_secret_msb` | ~~planned MSB~~ → secret sentinel 覆盖面由 #24 assertion-level ID 接管 | 已退役 |
| `planned_audit_ops_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_audit_query_seeded` / `planned_direct_audit_tail_seeded` / `planned_direct_audit_replay_trace` / `planned_direct_audit_attestation_match` / `planned_direct_audit_replay_unknown_turn` / `planned_direct_audit_query_unknown_session` / `planned_direct_lancet_status_seeded` assertion-level #24 ID | 已退役 |
| `planned_session_graph_msb` | ~~planned MSB~~ → 拆分为 `planned_direct_branch_listing_isolation` / `planned_direct_checkout_isolation` / `planned_direct_branch_create_isolation` / `planned_direct_reset_isolation` / `planned_direct_diff_isolation` assertion-level #24 ID | 已退役 |
| `planned_session_history_msb` | ~~planned MSB~~ → `planned_direct_session_log_turn` assertion-level #24 ID | 已退役 |

### 汇总

| 决定 | 条数 |
|---|---:|
| 保留 | 1337 |
| 提升 | 86 |
| 删除 | 26 |
| **合计** | **1449** |

#### 按 crate

| crate | 保留 | 提升 | 删除 | 合计 |
|---|---:|---:|---:|---:|
| `lattice-api` | 223 | 25 | 12 | 260 |
| `lattice-cli` | 118 | 57 | 10 | 185 |
| `lattice-workspace` | 158 | 0 | 0 | 158 |
| `lancet-authority` | 135 | 0 | 0 | 135 |
| `lattice-llm` | 133 | 0 | 0 | 133 |
| `lancet-native` | 107 | 0 | 0 | 107 |
| `lattice-profile` | 96 | 0 | 1 | 97 |
| `lattice-agent` | 68 | 3 | 0 | 71 |
| `lattice-session` | 69 | 0 | 2 | 71 |
| `lattice-system-harness` | 56 | 1 | 0 | 57 |
| `lancet-protocol` | 36 | 0 | 0 | 36 |
| `lancet-audit` | 34 | 0 | 0 | 34 |
| `lancet-catalog` | 31 | 0 | 0 | 31 |
| `xtask` | 26 | 0 | 0 | 26 |
| `lancet-api` | 17 | 0 | 1 | 18 |
| `net-guard` | 15 | 0 | 0 | 15 |
| `lancet-cep` | 7 | 0 | 0 | 7 |
| `lancet-dispatch` | 5 | 0 | 0 | 5 |
| `lattice-tui` | 3 | 0 | 0 | 3 |

### 4/6 建议执行批次（引用本清单，不在本票执行）

1. **CLI/config**：`lattice-cli` 提升/删除项 → `fast_cli_help` / `planned_session_cli` / `planned_config_cli` / `planned_audit_cli`
2. **session/audit**：`lattice-api` session 面 + audit 相关提升项
3. **turn/Upper**：`lattice-api` / `lattice-agent` 中 Fake LLM + DeterministicWorkspace 路径 → MSB 场景
4. **provider/安全**：provider 失败与 secret 场景；局部安全测试保持不动
5. **其余 crate**：仅处理清单中标记删除的源码扫描项；局部不变量批次默认跳过

---



---

## 1.5 #23 最终执行状态（2026-07-19）

> #22 的判决书在 #23 按产品面分批执行。本节是 **最终状态**：fast residual 归零、
> MSB 词汇退役、44 个 assertion-level #24 场景 ID 精确登记。历史执行批次
> （round-1..6、#40 状态更新）已折叠为下表的「已执行」与「精确移交 #24」两段；
> 旧的「MSB 项需 KVM 通道」延期表自 #40 起失效，保留为历史时明确标 obsolete。

### Fast residual：0

- 切片 1：`fast_lancet_cli` 接管 `cep_status_missing_db_returns_empty_state`（1 条）并删除原内部测试。
- 切片 2：`fast_session_cli` 接管 `project_branch_creation_json_output_includes_branch` / `project_branch_persists_through_reopen`（2 条）并删除原内部测试；另删 2 条已完整接管的 smoke（`smoke_session_status_list_shows_seeded_session` / `smoke_session_status_specific_session`）。

### #40 骨架已完成接管

| replacement scenario | 删除/接管的内部测试 | superset 依据 |
|---|---|---|
| `direct_local_plain_text_turn_commits_and_exit_zero` | `msb_plain_text_turn_commits_and_exit_zero`（历史 harness identity，keep→elevate） | #40 落地 plain-text 完整 turn + exit 0 + session log commit；file/line 修正为 `direct_local_turn.rs:108` |

### 11 条退役契约补记 delete（#37–#40 已删除产品面，不向 #24 虚构 replacement）

| ledger identity | 退役依据 |
|---|---|
| `stale_verification_reaches_no_promotion_effects` | #37–#40 删除 Upper/promotion 面 |
| `success_uses_real_session_callback_and_materializes_lower` | #37–#40 删除 Upper/promotion 面 |
| `promotion_module_documents_activation_requirement` | #37–#40 删除 Upper/promotion 面 |
| `prune_one_session_does_not_remove_another` | #37–#40 删除 prune/snapshot 面 |
| `prune_idempotent_second_call_returns_zero_released` | #37–#40 删除 prune/snapshot 面 |
| `prune_session_snapshots_skips_pending_worktree_snapshot` | #37–#40 删除 prune/snapshot 面 |
| `cleanup_status_error_yields_error_aborted` | #37–#40 删除 turn lifecycle Upper 面 |
| `upper_routing_e2e` | #37–#40 删除 Upper routing 面 |
| `adapter_failure_has_no_lower_fallback` | #37–#40 删除 Upper/adapter 面 |
| `generated_profiles_are_executable_host_only` | #37–#40 删除 host-only profile 面 |
| `microsandbox_upper_routing` | #37–#40 删除 MSB/Upper routing 面 |

### 精确移交 #24（44 行 assertion-level scenario ID；禁止 family fallback）

| exact planned scenario ID | ledger identity | assertion bundle |
|---|---|---|
| `planned_direct_message_assembly` | `agent_loop_builds_messages_from_transcript_and_user_input` | AgentLoop 从 transcript + user input 组装消息顺序/内容 |
| `planned_direct_tool_error_payload` | `tool_execution_error_structured_payload_preserved` | CEP tool error 结构化 payload 保留 |
| `planned_direct_streaming_chat_compat` | `streaming_backward_compat_fake_llm_chat_only_still_works` | Fake LLM chat-only streaming 向后兼容 |
| `planned_direct_basic_completed_event_contract` | `stream_turn_live_with_agent_basic_completed` | TurnStarted/TurnCommitting 事件序 + started/committed session ID 相等 + 无 failed/cancelled |
| `planned_direct_live_error_commit` | `stream_turn_live_with_agent_error_commits_error` | live turn error 提交 error commit |
| `planned_direct_live_event_order` | `stream_turn_live_with_agent_event_ordering` | live turn 事件顺序 |
| `planned_direct_provider_startup_failure` | `stream_turn_live_with_agent_startup_failure_no_stream` | provider 启动失败不产生 stream |
| `planned_direct_delta_only_commit` | `stream_turn_live_with_agent_commit_stores_delta_only` | commit 只存储 delta |
| `planned_direct_branch_population` | `stream_turn_live_with_agent_branch_populated` | branch 在 turn 后被填充 |
| `planned_direct_commit_failure` | `stream_turn_live_with_agent_commit_failure_deterministic` | commit 失败确定性（取代旧 `planned_resume_msb` 的 commit-failure 半） |
| `planned_direct_receiver_drop_commit` | `stream_turn_live_with_agent_receiver_drop_does_not_cancel` | receiver drop 不取消 commit |
| `planned_direct_slow_observer_commit` | `stream_turn_live_with_agent_slow_observer_still_commits` | 慢 observer 仍 commit |
| `planned_direct_tool_activity_before_commit` | `stream_turn_emits_tool_activity_before_committed` | tool activity 在 committed 前发出 |
| `planned_direct_stream_error_commit` | `stream_turn_error_still_commits_error` | stream error 仍 commit error |
| `planned_direct_cancelled_commit` | `stream_turn_cancelled_commits_cancelled` | cancelled commit 提交 cancelled |
| `planned_direct_partial_output_error_commit` | `stream_turn_error_after_partial_commits_error` | partial output 后 error 仍 commit error |
| `planned_direct_cep_lifecycle` | `stream_turn_emits_cep_lifecycle_events` | CEP lifecycle 事件 |
| `planned_direct_turn_audit_replay` | `full_turn_audit_rows_and_replay_fragment` | 完整 turn audit rows + replay fragment |
| `planned_direct_partial_token_parity` | `consistency_error_after_partial_token` | partial token 一致性 |
| `planned_direct_tool_call_parity` | `consistency_tool_call_path` | tool call path 一致性 |
| `planned_direct_activity_commit_order` | `stream_turn_emits_activity_before_commit` | activity 在 commit 前发出 |
| `planned_direct_completed_disposition_parity` | `consistency_completed_disposition_parity` | completed disposition 一致性 |
| `planned_direct_error_disposition_parity` | `consistency_error_disposition_parity` | error disposition 一致性 |
| `planned_direct_workspace_read_turn` | `usage_run_turn_can_read_workspace_file_with_default_profile` | default profile run_turn 读取 workspace 文件 |
| `planned_direct_routed_provider_event` | `usage_stream_turn_model_started_includes_routed_provider` | model_started 含 routed provider |
| `planned_direct_reopen_previous_context` | `usage_resume_session_includes_previous_turn_context` | resume session 含 previous turn context（取代旧 `planned_resume_msb` 的 reopen 半） |
| `planned_direct_fetch_capability_denial` | `public_https_policy_requires_fetch_capability` | https public fetch 需 fetch capability，拒绝缺 capability |
| `planned_direct_disabled_fetch_denial` | `rejects_profile_disabled_web_fetch` | profile 禁用 web fetch 时拒绝 |
| `planned_direct_audit_query_seeded` | `audit_query_prints_fragment_for_seeded_db` | audit query seeded DB fragment |
| `planned_direct_audit_tail_seeded` | `audit_tail_streams_rows_for_seeded_db` | audit tail seeded DB rows |
| `planned_direct_audit_query_unknown_session` | `audit_query_empty_result_for_unknown_session` | audit query unknown session 空结果 |
| `planned_direct_audit_replay_trace` | `audit_replay_reconstructs_turn_trace_for_seeded_db` | audit replay 重构 turn trace（seeded DB） |
| `planned_direct_audit_attestation_match` | `audit_replay_reports_stored_attestation_match` | audit replay 报告 stored attestation 匹配 |
| `planned_direct_audit_replay_unknown_turn` | `audit_replay_errors_for_unknown_turn` | audit replay unknown turn 报错 |
| `planned_direct_lancet_status_seeded` | `cep_status_seeded_audit_json` | lancet status seeded audit JSON |
| `planned_direct_provider_timeout` | `stream_timeout_fires_after_duration` | provider stream timeout 触发 |
| `planned_direct_existing_session_run_result` | `run_integration_commits_existing_session` | RunTurnResult.assistant_output/status（existing session） |
| `planned_direct_canonical_workspace_root` | `tool_executor_uses_workspace_root_not_profile_package_dir` | tool executor 使用 workspace root 而非 profile package dir |
| `planned_direct_branch_listing_isolation` | `branch_listing_reads_only_addressed_session_refs` | branch listing 只读 addressed session refs |
| `planned_direct_checkout_isolation` | `checkout_changes_only_addressed_session_head` | checkout 只改 addressed session HEAD |
| `planned_direct_branch_create_isolation` | `branch_create_affects_only_addressed_session` | branch create 只影响 addressed session |
| `planned_direct_reset_isolation` | `reset_changes_only_addressed_session_branch_pointer` | reset 只移动 addressed session branch pointer |
| `planned_direct_diff_isolation` | `diff_resolves_refs_inside_addressed_session_only` | diff refs 在 addressed session 内解析 |
| `planned_direct_session_log_turn` | `smoke_session_log_shows_seeded_turn` | session log 显示 seeded turn |

### MSB 词汇退役

#40 删除 Upper/MSB/KVM lane 与 `LATTICE_HARNESS_MSB` 门禁通道；active ledger 只使用
`direct_local_binary` 目标层与 `*_direct` / `direct_local_*` replacement 词汇。
`real_microsandbox` / `msb_full_turn` / `planned_*_msb` 在 machine ledger
（`docs/test-triage-decisions.json`）中为零；在本文件仅保留于上方「历史/obsolete」
场景目录段与旧的 round-4 复审记录中，且明确标 obsolete，不与当前 active 状态并列。

### layer 分布（最终）

| 目标层 | 条数 |
|---|---:|
| 局部不变量 | 1358 |
| 快速二进制黑盒 | 45 |
| Direct-Local 外部二进制 | 46 |
| **合计** | **1449** |

### 决定分布（最终）

| 决定 | 条数 |
|---|---:|
| 保留 | 1337 |
| 提升 | 86 |
| 删除 | 26 |
| **合计** | **1449** |
## 2. 逐条清单

### `lancet-api`（保留 17 / 提升 0 / 删除 1）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `query_audit_reads_store_without_cep_server` | `crates/lancet-api/src/audit.rs:450` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `tail_audit_paginates_large_result_without_cep_server` | `crates/lancet-api/src/audit.rs:463` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `query_audit_errors_when_store_missing` | `crates/lancet-api/src/audit.rs:505` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `replay_audit_returns_trace_for_turn` | `crates/lancet-api/src/audit.rs:513` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `replay_audit_matches_stored_attestation` | `crates/lancet-api/src/audit.rs:529` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `query_audit_excludes_unfinalized_rows_during_active_turn` | `crates/lancet-api/src/audit.rs:580` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `operator_audit_reads_concurrent_with_active_writer` | `crates/lancet-api/src/audit.rs:630` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `operator_audit_module_does_not_reference_cep_server_types` | `crates/lancet-api/src/audit.rs:745` | 删除 | 局部不变量 | n/a — 无替代场景（源码字符串扫描；边界由 xtask/依赖图负责） | 扫描源码字符串/import 的架构检查；删除（RFC #19 决定六） |
| `query_audit_reads_legacy_table_missing_optional_columns` | `crates/lancet-api/src/audit.rs:768` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `query_audit_reads_legacy_table_without_attestation_records` | `crates/lancet-api/src/audit.rs:830` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `external_audit_distinct_project_keys` | `crates/lancet-api/src/config.rs:176` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `external_audit_symlink_canonical_identity` | `crates/lancet-api/src/config.rs:193` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `external_audit_external_for_project_in_resolves_under_state_root` | `crates/lancet-api/src/config.rs:205` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `external_audit_invalid_canonical_identity_errors` | `crates/lancet-api/src/config.rs:219` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `external_audit_imports_legacy_read_only_once` | `crates/lancet-api/src/config.rs:227` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `runtime_config_defaults_have_null_execution` | `crates/lancet-api/src/runtime.rs:246` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `production_runtime_rejects_retired_exec_command_run_as_method_not_found` | `crates/lancet-api/src/runtime.rs:259` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |
| `production_runtime_rejects_retired_mutate_batch_as_method_not_found` | `crates/lancet-api/src/runtime.rs:313` | 保留 | 局部不变量 | — | LancetRuntime 组合/配置/audit 只读 API 局部契约；保留 |

### `lancet-audit`（保留 34 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `open_rejects_symlinked_audit_parent` | `crates/lancet-audit/src/audit_tests.rs:32` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `open_rejects_symlinked_audit_database_file` | `crates/lancet-audit/src/audit_tests.rs:53` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `for_production_dispatch_labels_known_native_and_unknown_unrouted` | `crates/lancet-audit/src/audit_tests.rs:79` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_success_appends_one_row_with_required_fields` | `crates/lancet-audit/src/audit_tests.rs:93` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_error_path_finalizes_ended_at` | `crates/lancet-audit/src/audit_tests.rs:132` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_unknown_method_and_cancel_paths_finalize` | `crates/lancet-audit/src/audit_tests.rs:164` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_rows_store_summaries_not_full_payloads` | `crates/lancet-audit/src/audit_tests.rs:217` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_failed_insert_does_not_increment_pending` | `crates/lancet-audit/src/audit_tests.rs:253` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_failed_finalize_still_decrements_pending` | `crates/lancet-audit/src/audit_tests.rs:272` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_native_and_unrouted_rows_coexist` | `crates/lancet-audit/src/audit_tests.rs:302` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_metadata_json_excludes_full_payloads` | `crates/lancet-audit/src/audit_tests.rs:368` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_effective_boundary_metadata_round_trips_compactly` | `crates/lancet-audit/src/audit_tests.rs:400` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `impact_kind_label_round_trips_all_variants` | `crates/lancet-audit/src/audit_tests.rs:492` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `start_call_persists_model_and_runtime_columns` | `crates/lancet-audit/src/audit_tests.rs:511` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `attestation_round_trip` | `crates/lancet-audit/src/audit_tests.rs:543` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `query_rows_filters_by_session_and_method` | `crates/lancet-audit/src/audit_tests.rs:561` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `query_rows_filters_by_verdict` | `crates/lancet-audit/src/audit_tests.rs:620` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `query_rows_filters_by_capability` | `crates/lancet-audit/src/audit_tests.rs:692` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `query_rows_applies_database_limit_and_excluded_method_prefix` | `crates/lancet-audit/src/audit_tests.rs:744` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `query_rows_excludes_unfinalized_rows` | `crates/lancet-audit/src/audit_tests.rs:791` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `merge_finalize_metadata_adds_required_capability` | `crates/lancet-audit/src/audit_tests.rs:813` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `cep_audit_await_pending_waits_for_finalization` | `crates/lancet-audit/src/audit_tests.rs:825` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `rejection_stage_serializes_stable_snake_case_labels` | `crates/lancet-audit/src/audit_tests.rs:855` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `rejection_stage_filters_rows_by_metadata_json` | `crates/lancet-audit/src/audit_tests.rs:872` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `import_legacy_read_only_if_empty_copies_both_tables` | `crates/lancet-audit/src/audit_tests.rs:974` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `import_legacy_read_only_if_empty_is_noop_when_destination_has_rows` | `crates/lancet-audit/src/audit_tests.rs:1014` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `build_statement_lists_materials` | `crates/lancet-audit/src/provenance/attestation.rs:145` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `source_audit_range_hash_is_stable` | `crates/lancet-audit/src/provenance/attestation.rs:191` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `canonicalize_strips_timestamps` | `crates/lancet-audit/src/provenance/canonicalize.rs:76` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `project_rows_builds_activity_per_row` | `crates/lancet-audit/src/provenance/projector.rs:278` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `parse_impact_kind_warns_on_unknown_label` | `crates/lancet-audit/src/provenance/projector.rs:297` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `project_turn_trace_orders_calls` | `crates/lancet-audit/src/provenance/projector.rs:310` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `boundary_observed_projection` | `crates/lancet-audit/src/provenance/projector.rs:323` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |
| `preflight_and_policy_metadata_project_to_activity_attributes` | `crates/lancet-audit/src/provenance/projector.rs:361` | 保留 | 局部不变量 | — | audit 持久化/过滤/provenance 投影局部契约；保留 |

### `lancet-authority`（保留 135 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `observation_within_region_returns_ok` | `crates/lancet-authority/src/effect/checker.rs:220` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `denied_effect_kind_rejects_authorized_handler_effect` | `crates/lancet-authority/src/effect/checker.rs:239` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `mutation_in_observation_method_returns_effect_violation` | `crates/lancet-authority/src/effect/checker.rs:275` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `probe_and_guard_effects_are_not_charged` | `crates/lancet-authority/src/effect/checker.rs:304` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `verify_effect_is_not_charged` | `crates/lancet-authority/src/effect/checker.rs:334` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `empty_region_with_any_handler_effect_returns_violation` | `crates/lancet-authority/src/effect/checker.rs:355` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `read_within_readpath_region_returns_ok` | `crates/lancet-authority/src/effect/checker.rs:376` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `read_outside_readpath_region_returns_violation` | `crates/lancet-authority/src/effect/checker.rs:402` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `mixed_charged_and_uncharged_only_charged_matters` | `crates/lancet-authority/src/effect/checker.rs:428` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `violation_populates_all_effect_error_data_fields` | `crates/lancet-authority/src/effect/checker.rs:455` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `fresh_ledger_has_empty_snapshot` | `crates/lancet-authority/src/effect/ledger.rs:190` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `record_then_snapshot_returns_effect` | `crates/lancet-authority/src/effect/ledger.rs:196` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `shared_arc_record_via_clone_a_read_via_clone_b` | `crates/lancet-authority/src/effect/ledger.rs:209` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `effect_source_has_all_four_variants` | `crates/lancet-authority/src/effect/ledger.rs:223` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observed_scope_variants` | `crates/lancet-authority/src/effect/ledger.rs:243` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `multiple_effects_accumulate_in_order` | `crates/lancet-authority/src/effect/ledger.rs:259` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `mutex_poison_recovery` | `crates/lancet-authority/src/effect/ledger.rs:277` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `fresh_slot_returns_none` | `crates/lancet-authority/src/effect/ledger.rs:308` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `set_then_get_returns_region` | `crates/lancet-authority/src/effect/ledger.rs:314` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `shared_arc_set_via_clone_a_read_via_clone_b` | `crates/lancet-authority/src/effect/ledger.rs:324` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `mutex_poison_recovery_on_slot` | `crates/lancet-authority/src/effect/ledger.rs:338` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `empty_region_contains_nothing` | `crates/lancet-authority/src/effect/region.rs:240` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `effect_kind_set_to_btreeset_conversion` | `crates/lancet-authority/src/effect/region.rs:257` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `effect_kind_set_to_btreeset_empty` | `crates/lancet-authority/src/effect/region.rs:269` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `effect_kind_set_to_btreeset_single` | `crates/lancet-authority/src/effect/region.rs:276` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `from_effect_kind_set_materializes_all_kinds` | `crates/lancet-authority/src/effect/region.rs:286` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `exact_file_pattern` | `crates/lancet-authority/src/guard.rs:691` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `dir_glob_pattern` | `crates/lancet-authority/src/guard.rs:697` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `simple_prefix_normalized_to_dir_glob` | `crates/lancet-authority/src/guard.rs:706` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `covers_directory_glob_children_and_self` | `crates/lancet-authority/src/guard.rs:717` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `covers_single_component_directory` | `crates/lancet-authority/src/guard.rs:726` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `multi_component_no_extension_is_exact_file_not_directory` | `crates/lancet-authority/src/guard.rs:732` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `covers_rejects_malformed_inputs` | `crates/lancet-authority/src/guard.rs:747` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_dot_dot` | `crates/lancet-authority/src/guard.rs:754` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_nul` | `crates/lancet-authority/src/guard.rs:760` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_empty` | `crates/lancet-authority/src/guard.rs:766` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_absolute_pattern` | `crates/lancet-authority/src/guard.rs:772` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `nested_dir_glob` | `crates/lancet-authority/src/guard.rs:778` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `trailing_slash_directory_prefix_normalized_to_dir_glob` | `crates/lancet-authority/src/guard.rs:787` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_unsupported_glob_syntax` | `crates/lancet-authority/src/guard.rs:796` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `reject_negation_syntax` | `crates/lancet-authority/src/guard.rs:802` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `exact_file_match` | `crates/lancet-authority/src/guard.rs:810` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `dir_glob_denies_child` | `crates/lancet-authority/src/guard.rs:821` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `simple_prefix_denies_child` | `crates/lancet-authority/src/guard.rs:833` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `target_not_denied_is_allowed` | `crates/lancet-authority/src/guard.rs:845` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `target_outside_project_root_returns_guard_denied` | `crates/lancet-authority/src/guard.rs:856` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `pattern_with_dot_dot_rejected` | `crates/lancet-authority/src/guard.rs:865` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `nul_in_target_rejected` | `crates/lancet-authority/src/guard.rs:874` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `symlink_escape_returns_guard_denied` | `crates/lancet-authority/src/guard.rs:884` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `multiple_patterns_first_deny_wins` | `crates/lancet-authority/src/guard.rs:899` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `no_patterns_allows_all` | `crates/lancet-authority/src/guard.rs:912` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `new_file_not_denied_succeeds` | `crates/lancet-authority/src/guard.rs:923` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `evaluate_with_default_hook_returns_evidence` | `crates/lancet-authority/src/guard.rs:941` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `evaluate_with_external_hook_returns_no_evidence` | `crates/lancet-authority/src/guard.rs:969` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_allowed_returns_ok` | `crates/lancet-authority/src/guard.rs:1002` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_missing_capability_to_capability_not_granted` | `crates/lancet-authority/src/guard.rs:1007` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_replan_required_to_replan_required` | `crates/lancet-authority/src/guard.rs:1019` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_budget_exceeded_to_reserved_with_replan` | `crates/lancet-authority/src/guard.rs:1031` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_non_determinable_to_guard_denied_unknown` | `crates/lancet-authority/src/guard.rs:1043` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_human_review_to_guard_denied_unknown` | `crates/lancet-authority/src/guard.rs:1055` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_network_denied_to_guard_denied` | `crates/lancet-authority/src/guard.rs:1066` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_path_outside_scope_to_replan_required` | `crates/lancet-authority/src/guard.rs:1077` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_capability_source_not_allowed_to_capability_not_granted` | `crates/lancet-authority/src/guard.rs:1089` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `normalize_lexical_dot_returns_dot_sentinel` | `crates/lancet-authority/src/guard.rs:1100` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `normalize_lexical_dotslash_prefix_returns_tail` | `crates/lancet-authority/src/guard.rs:1110` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `path_facts_accepts_workspace_root` | `crates/lancet-authority/src/guard.rs:1116` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `path_facts_rejects_mismatched_path` | `crates/lancet-authority/src/guard.rs:1131` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `path_facts_rejects_non_canonical_containment` | `crates/lancet-authority/src/guard.rs:1146` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `path_facts_rejects_denied_pattern` | `crates/lancet-authority/src/guard.rs:1161` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `path_facts_allows_when_no_denied_patterns` | `crates/lancet-authority/src/guard.rs:1180` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `map_policy_config_invalid_to_reserved` | `crates/lancet-authority/src/guard.rs:1196` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `verdict_allowed` | `crates/lancet-authority/src/guard.rs:1208` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `verdict_denied` | `crates/lancet-authority/src/guard.rs:1216` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `verdict_human_review` | `crates/lancet-authority/src/guard.rs:1231` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `cep_budget_rejects_aggregate_observation_exhaustion` | `crates/lancet-authority/src/policy/budget.rs:143` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `cep_budget_snapshot_without_audit_store` | `crates/lancet-authority/src/policy/budget.rs:156` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `cep_budget_todo_op_cap` | `crates/lancet-authority/src/policy/budget.rs:170` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `cep_budget_todo_ops_charge_by_item_count` | `crates/lancet-authority/src/policy/budget.rs:184` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_allows_workspace_read_file` | `crates/lancet-authority/src/policy/default.rs:708` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_allows_create_with_no_denied_patterns` | `crates/lancet-authority/src/policy/default.rs:723` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_denies_command_run_as_unknown_method` | `crates/lancet-authority/src/policy/default.rs:738` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_denies_web_fetch_with_disallowed_host` | `crates/lancet-authority/src/policy/default.rs:763` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_denies_observation_over_budget` | `crates/lancet-authority/src/policy/default.rs:787` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `default_hook_returns_denied_for_unknown_method` | `crates/lancet-authority/src/policy/default.rs:813` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `policy_config_defaults_produces_valid_config` | `crates/lancet-authority/src/policy/default.rs:835` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `policy_config_merge_precedence_external_overrides_defaults` | `crates/lancet-authority/src/policy/default.rs:849` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `decide_resolved_is_pure_same_input_same_output` | `crates/lancet-authority/src/policy/default.rs:865` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `decide_with_evidence_contains_rich_facts` | `crates/lancet-authority/src/policy/default.rs:885` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `policy_hook_decide_returns_stable_decision` | `crates/lancet-authority/src/policy/default.rs:902` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `policy_config_to_cep_policy_config` | `crates/lancet-authority/src/policy/default.rs:917` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `external_policy_file_deserializes_from_toml` | `crates/lancet-authority/src/policy/default.rs:930` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_denies_localhost_by_default` | `crates/lancet-authority/src/policy/network.rs:220` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_localhost_requires_allowlist_entry` | `crates/lancet-authority/src/policy/network.rs:227` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_allows_localhost_with_matching_allowlist` | `crates/lancet-authority/src/policy/network.rs:237` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_allowlist_only_public_https` | `crates/lancet-authority/src/policy/network.rs:248` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_denies_private_ip_literal` | `crates/lancet-authority/src/policy/network.rs:259` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_denies_metadata_ip` | `crates/lancet-authority/src/policy/network.rs:266` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_denies_non_http_scheme` | `crates/lancet-authority/src/policy/network.rs:274` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_allows_public_https_host` | `crates/lancet-authority/src/policy/network.rs:281` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `network_policy_allowed_hosts_narrows_access` | `crates/lancet-authority/src/policy/network.rs:289` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observation_policy_rejects_parent_traversal` | `crates/lancet-authority/src/policy/observation.rs:207` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observation_policy_allows_relative_inside_root` | `crates/lancet-authority/src/policy/observation.rs:221` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observation_policy_allows_absolute_inside_root` | `crates/lancet-authority/src/policy/observation.rs:230` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observation_policy_denies_absolute_outside_root` | `crates/lancet-authority/src/policy/observation.rs:242` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `observation_policy_denies_symlink_escape` | `crates/lancet-authority/src/policy/observation.rs:259` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `backend_safety_net_denial_is_permission_family` | `crates/lancet-authority/src/policy/observation.rs:276` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_read_file_extracts_path` | `crates/lancet-authority/src/policy/resolved.rs:332` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_create_extracts_path` | `crates/lancet-authority/src/policy/resolved.rs:349` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_edit_extracts_path` | `crates/lancet-authority/src/policy/resolved.rs:363` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_grep_extracts_optional_path` | `crates/lancet-authority/src/policy/resolved.rs:383` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_list_directory_extracts_path` | `crates/lancet-authority/src/policy/resolved.rs:396` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_workspace_find_files_extracts_path` | `crates/lancet-authority/src/policy/resolved.rs:409` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_command_run_errors_as_unknown_method` | `crates/lancet-authority/src/policy/resolved.rs:421` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_web_fetch_extracts_url_facts` | `crates/lancet-authority/src/policy/resolved.rs:436` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_session_todo_write_identifies_operation` | `crates/lancet-authority/src/policy/resolved.rs:454` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_unknown_method_errors` | `crates/lancet-authority/src/policy/resolved.rs:473` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolve_path_traversal_rejected` | `crates/lancet-authority/src/policy/resolved.rs:486` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `resolved_policy_input_has_no_serialize` | `crates/lancet-authority/src/policy/resolved.rs:499` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `guard_decide_fuzz_no_panic_and_determinism` | `crates/lancet-authority/tests/guard_decide_fuzz.rs:181` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `guard_decide_fuzz_deny_has_reason` | `crates/lancet-authority/tests/guard_decide_fuzz.rs:190` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `guard_decide_fuzz_biased_allowed_stable` | `crates/lancet-authority/tests/guard_decide_fuzz.rs:205` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `guard_decide_regression_budget_exhausted_deny_message` | `crates/lancet-authority/tests/guard_decide_fuzz.rs:217` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `guard_decide_regression_unknown_method_nondeterminable_deny` | `crates/lancet-authority/tests/guard_decide_fuzz.rs:246` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `all_catalog_methods_have_coverage` | `crates/lancet-authority/tests/policy_coverage.rs:50` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `method_matrix_covers_all_methods` | `crates/lancet-authority/tests/policy_coverage.rs:73` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `allows_observation_without_denied_patterns` | `crates/lancet-authority/tests/policy_tests.rs:41` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `allows_mutation_with_no_denied_patterns` | `crates/lancet-authority/tests/policy_tests.rs:53` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `allows_mutation_not_in_deny_list` | `crates/lancet-authority/tests/policy_tests.rs:66` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `denies_retired_command_run_as_unknown_method` | `crates/lancet-authority/tests/policy_tests.rs:77` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `denies_network_host` | `crates/lancet-authority/tests/policy_tests.rs:95` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `denies_observation_budget_exhausted` | `crates/lancet-authority/tests/policy_tests.rs:111` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `denies_todo_budget_exhausted` | `crates/lancet-authority/tests/policy_tests.rs:131` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `decision_is_deterministic` | `crates/lancet-authority/tests/policy_tests.rs:145` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `workspace_policy_validates_denied_patterns` | `crates/lancet-authority/tests/policy_tests.rs:158` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |
| `workspace_policy_applies_denied_patterns_to_mutate_region` | `crates/lancet-authority/tests/policy_tests.rs:167` | 保留 | 局部不变量 | — | policy/capability/Guard L1/effect 纯语义局部不变量；保留 |

### `lancet-catalog`（保留 31 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `catalog_contains_expected_methods_in_stable_order` | `crates/lancet-catalog/src/catalog.rs:67` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_method_names_are_unique` | `crates/lancet-catalog/src/catalog.rs:73` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_method_names_are_full_dotted_names` | `crates/lancet-catalog/src/catalog.rs:81` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_descriptors_have_non_empty_descriptions` | `crates/lancet-catalog/src/catalog.rs:97` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_descriptors_have_params_schema_object` | `crates/lancet-catalog/src/catalog.rs:108` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_descriptors_declare_base_authority_requirements` | `crates/lancet-catalog/src/catalog.rs:121` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_impact_and_capability_match_method_constants` | `crates/lancet-catalog/src/catalog.rs:146` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_len_is_5` | `crates/lancet-catalog/src/catalog.rs:175` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `default_catalog_has_no_inspect_or_navigate` | `crates/lancet-catalog/src/catalog.rs:180` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `scaffold_catalog_contains_inspect_and_navigate` | `crates/lancet-catalog/src/catalog.rs:197` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `scaffold_descriptors_have_object_params_schema` | `crates/lancet-catalog/src/catalog.rs:203` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `scaffold_result_schema_includes_status_semantic_stage_and_routed` | `crates/lancet-catalog/src/catalog.rs:216` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_default_snapshot_unchanged_with_all_features` | `crates/lancet-catalog/src/catalog.rs:239` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_excludes_audit_agent_methods` | `crates/lancet-catalog/src/catalog.rs:262` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_retires_exec_command_run` | `crates/lancet-catalog/src/catalog.rs:275` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_retires_mutate_batch` | `crates/lancet-catalog/src/catalog.rs:286` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_len_matches_runtime_catalog` | `crates/lancet-catalog/src/catalog.rs:297` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `catalog_default_snapshot_scaffold_is_separate` | `crates/lancet-catalog/src/catalog.rs:303` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `audit_replay_params_require_turn_id_field` | `crates/lancet-catalog/src/methods/audit.rs:68` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `web_fetch_name_is_dotted` | `crates/lancet-catalog/src/methods/external.rs:58` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `fetch_description_explains_host_allowlist_and_no_retry` | `crates/lancet-catalog/src/methods/external.rs:65` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `fetch_result_has_additive_truncated_field` | `crates/lancet-catalog/src/methods/external.rs:71` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `all_catalog_method_names_are_dotted_without_slashes` | `crates/lancet-catalog/src/methods/mod.rs:48` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `no_legacy_slash_method_names_in_protocol` | `crates/lancet-catalog/src/methods/mod.rs:63` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `session_todo_write_name_is_dotted` | `crates/lancet-catalog/src/methods/session.rs:65` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `workspace_method_names_are_full_dotted_names` | `crates/lancet-catalog/src/methods/workspace.rs:242` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `read_file_params_schema_has_path_property` | `crates/lancet-catalog/src/methods/workspace.rs:250` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `read_file_round_trips` | `crates/lancet-catalog/src/methods/workspace.rs:258` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `create_params_defaults_create_parents_to_false` | `crates/lancet-catalog/src/methods/workspace.rs:271` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `create_params_schema_exposes_create_parents` | `crates/lancet-catalog/src/methods/workspace.rs:279` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |
| `mutation_descriptions_point_to_write_scope_and_directory_patterns` | `crates/lancet-catalog/src/methods/workspace.rs:287` | 保留 | 局部不变量 | — | typed method contracts/schema/descriptor 局部不变量；保留 |

### `lancet-cep`（保留 7 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `tool_definition_roundtrip` | `crates/lattice-llm/src/types.rs:325` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `projection_length_equals_catalog_len` | `crates/lancet-cep/src/tool.rs:205` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |
| `projected_names_match_catalog_exactly` | `crates/lancet-cep/src/tool.rs:211` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |
| `enabled_filter_rejects_unknown` | `crates/lancet-cep/src/tool.rs:219` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |
| `enabled_filter_uses_exact_names` | `crates/lancet-cep/src/tool.rs:227` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |
| `catalog_exposure_collects_base_authority_from_descriptors` | `crates/lancet-cep/src/tool.rs:237` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |
| `inventory_only_exposure_has_no_authority_semantics` | `crates/lancet-cep/src/tool.rs:250` | 保留 | 局部不变量 | — | lancet-cep 局部不变量（wire/安全/dispatch/投影）；保留 |

### `lancet-dispatch`（保留 5 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `registry_typed_handler_dispatch_returns_result` | `crates/lancet-dispatch/src/registry.rs:239` | 保留 | 局部不变量 | — | lancet-dispatch 局部不变量（wire/安全/dispatch/投影）；保留 |
| `registry_invalid_params_returns_32602` | `crates/lancet-dispatch/src/registry.rs:263` | 保留 | 局部不变量 | — | lancet-dispatch 局部不变量（wire/安全/dispatch/投影）；保留 |
| `registry_unknown_method_returns_32601` | `crates/lancet-dispatch/src/registry.rs:278` | 保留 | 局部不变量 | — | lancet-dispatch 局部不变量（wire/安全/dispatch/投影）；保留 |
| `registry_duplicate_registration_returns_error` | `crates/lancet-dispatch/src/registry.rs:293` | 保留 | 局部不变量 | — | lancet-dispatch 局部不变量（wire/安全/dispatch/投影）；保留 |
| `transport_round_trip_returns_typed_result` | `crates/lancet-dispatch/src/transport.rs:426` | 保留 | 局部不变量 | — | lancet-dispatch 局部不变量（wire/安全/dispatch/投影）；保留 |

### `lancet-native`（保留 107 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `apply_patch_engine_missing_hash_on_update_fails_before_any_write` | `crates/lancet-native/src/edit/apply.rs:904` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_missing_hash_on_delete_fails_before_any_write` | `crates/lancet-native/src/edit/apply.rs:926` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_hash_mismatch_fails_before_any_write` | `crates/lancet-native/src/edit/apply.rs:948` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_line_numbered_hashes_disambiguate_repeated_content` | `crates/lancet-native/src/edit/apply.rs:975` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_rejects_outputs_above_patch_write_limit` | `crates/lancet-native/src/edit/apply.rs:1005` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_write_stage_failure_rolls_back_already_written_files` | `crates/lancet-native/src/edit/apply.rs:1038` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_success_payload_includes_changed_file_manifest` | `crates/lancet-native/src/edit/apply.rs:1090` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `line_change_not_baseline_evidence_source` | `crates/lancet-native/src/edit/apply.rs:1135` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_failure_payload_keeps_changed_files_empty_with_attempts` | `crates/lancet-native/src/edit/apply.rs:1181` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `apply_patch_engine_allows_full_file_replacement_with_hashline_anchors` | `crates/lancet-native/src/edit/apply.rs:1209` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `content_hash_generates_blake3_prefix` | `crates/lancet-native/src/edit/hash.rs:65` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_old_hash_rejects_raw_hex_without_blake3_prefix` | `crates/lancet-native/src/edit/hash.rs:73` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `line_hash_uses_line_number_and_short_prefix` | `crates/lancet-native/src/edit/hash.rs:83` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_line_hash_rejects_full_hash` | `crates/lancet-native/src/edit/hash.rs:93` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_accepts_begin_update_delete` | `crates/lancet-native/src/edit/parser.rs:334` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_rejects_update_without_old_hash` | `crates/lancet-native/src/edit/parser.rs:376` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_rejects_delete_without_old_hash` | `crates/lancet-native/src/edit/parser.rs:394` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_rejects_add_file_operation` | `crates/lancet-native/src/edit/parser.rs:408` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_rejects_unterminated_patch_text` | `crates/lancet-native/src/edit/parser.rs:422` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `parse_patch_document_rejects_raw_hex_hashes` | `crates/lancet-native/src/edit/parser.rs:438` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `serializes_every_required_patch_error_code` | `crates/lancet-native/src/edit/types.rs:232` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `serializes_patch_diagnostic_coordinates` | `crates/lancet-native/src/edit/types.rs:252` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `serializes_patch_result_envelope_and_changed_file_manifest` | `crates/lancet-native/src/edit/types.rs:271` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `read_file_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:24` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `grep_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:46` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:67` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `edit_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:89` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `fetch_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:119` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `todo_write_effect_contract_positive` | `crates/lancet-native/src/effect_contract_tests/properties.rs:179` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `read_file_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:203` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `grep_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:224` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:244` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `edit_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:264` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `fetch_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:288` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `todo_write_effect_contract_negative` | `crates/lancet-native/src/effect_contract_tests/properties.rs:318` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `malicious_handler_caught` | `crates/lancet-native/src/effect_contract_tests/regression.rs:17` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_glob_pattern_within_region` | `crates/lancet-native/src/effect_contract_tests/regression.rs:77` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `runtime_backend_effects_are_declared_for_every_catalog_method` | `crates/lancet-native/src/effect_contract_tests/runtime_matrix.rs:31` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `build_policy_input_includes_session_id_and_budget` | `crates/lancet-native/src/enforcement/guard_l1.rs:204` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `human_review_records_approval_ready_evidence` | `crates/lancet-native/src/enforcement/guard_l1.rs:239` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_rejects_absolute` | `crates/lancet-native/src/execution_backend.rs:420` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_rejects_empty_and_nul` | `crates/lancet-native/src/execution_backend.rs:432` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_normalizes_repeated_separators` | `crates/lancet-native/src/execution_backend.rs:444` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_project_root_sentinel` | `crates/lancet-native/src/execution_backend.rs:450` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_git_is_syntactically_valid` | `crates/lancet-native/src/execution_backend.rs:461` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `workspace_path_rejects_upper_root_alias` | `crates/lancet-native/src/execution_backend.rs:467` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `debug_display_do_not_leak_host_paths` | `crates/lancet-native/src/execution_backend.rs:479` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `rejects_known_dns_rebinding_domains` | `crates/lancet-native/src/executor/web.rs:202` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `rejects_https_localhost` | `crates/lancet-native/src/executor/web.rs:223` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `allows_http_localhost` | `crates/lancet-native/src/executor/web.rs:229` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `rejects_private_ip_literal` | `crates/lancet-native/src/executor/web.rs:234` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_web_fetch_returns_real_status_not_hardcoded_200` | `crates/lancet-native/src/handlers/external.rs:294` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_web_fetch_truncates_over_cap_response` | `crates/lancet-native/src/handlers/external.rs:326` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_web_fetch_denies_localhost_by_default` | `crates/lancet-native/src/handlers/external.rs:371` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_web_fetch_turn_observation_budget_error` | `crates/lancet-native/src/handlers/external.rs:399` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_with_null_backend_returns_execution_failed` | `crates/lancet-native/src/handlers/mutation.rs:1115` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_in_scope_succeeds` | `crates/lancet-native/src/handlers/mutation.rs:1136` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_out_of_scope_returns_replan_required` | `crates/lancet-native/src/handlers/mutation.rs:1165` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `create_emits_guard_verdict_event` | `crates/lancet-native/src/handlers/mutation.rs:1186` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `edit_semantic_replace_computes_hashline_in_runtime` | `crates/lancet-native/src/handlers/mutation.rs:1400` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `edit_ambiguous_text_requires_occurrence` | `crates/lancet-native/src/handlers/mutation.rs:1443` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `edit_stale_expected_hash_records_preflight_replan_metadata` | `crates/lancet-native/src/handlers/mutation.rs:1474` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `mutation_create_uses_backend_only` | `crates/lancet-native/src/handlers/mutation.rs:1518` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_read_file_returns_typed_content_and_hash` | `crates/lancet-native/src/handlers/observation.rs:1213` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_read_file_rejects_parent_traversal_without_backend` | `crates/lancet-native/src/handlers/observation.rs:1232` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_read_file_single_read_budget_error` | `crates/lancet-native/src/handlers/observation.rs:1254` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_read_file_turn_budget_error` | `crates/lancet-native/src/handlers/observation.rs:1307` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_workspace_observation_grep_rejects_parent_traversal` | `crates/lancet-native/src/handlers/observation.rs:1360` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_workspace_observation_grep_invalid_regex` | `crates/lancet-native/src/handlers/observation.rs:1378` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_workspace_observation_grep_and_list_and_find` | `crates/lancet-native/src/handlers/observation.rs:1394` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_observation_records_requested_root_effect_scope_under_allowed_root` | `crates/lancet-native/src/handlers/observation.rs:1434` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_recursive_observation_does_not_follow_symlink_outside_allowed_root` | `crates/lancet-native/src/handlers/observation.rs:1469` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_workspace_observation_list_directory_rejects_parent_traversal` | `crates/lancet-native/src/handlers/observation.rs:1532` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_workspace_observation_truncates_grep_results` | `crates/lancet-native/src/handlers/observation.rs:1549` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `observation_policy_rejects_dotdot_in_handler_path` | `crates/lancet-native/src/handlers/observation.rs:1604` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `observation_returns_upper_not_lower` | `crates/lancet-native/src/handlers/observation.rs:1792` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `observation_policy_denial_before_preflight` | `crates/lancet-native/src/handlers/observation.rs:1819` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `production_registry_excludes_scaffold_methods` | `crates/lancet-native/src/handlers/scaffold.rs:125` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `scaffold_disabled_dispatch_returns_method_not_found` | `crates/lancet-native/src/handlers/scaffold.rs:133` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `scaffold_enabled_returns_not_yet_routed_without_budget_charge` | `crates/lancet-native/src/handlers/scaffold.rs:148` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `scaffold_transport_audit_uses_scaffold_stage_and_zero_budget` | `crates/lancet-native/src/handlers/scaffold.rs:173` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_todo_write_succeeds_without_workspace_envelope` | `crates/lancet-native/src/handlers/session_state.rs:182` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_todo_write_rejects_over_max_todo_ops` | `crates/lancet-native/src/handlers/session_state.rs:220` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_todo_write_rejects_many_todos_in_one_call_over_cap` | `crates/lancet-native/src/handlers/session_state.rs:261` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `native_todo_write_rejects_two_in_progress` | `crates/lancet-native/src/handlers/session_state.rs:294` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `command_boundary_metadata_survives_backend_invoked_update` | `crates/lancet-native/src/server/call_durable_metadata.rs:48` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `cep_native_registry_registers_full_catalog_once` | `crates/lancet-native/src/server/native_registry.rs:221` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `cep_native_registry_double_registration_fails` | `crates/lancet-native/src/server/native_registry.rs:233` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `cep_native_registry_native_error_returns_guard_denied` | `crates/lancet-native/src/server/native_registry.rs:242` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `cep_native_registry_labels_native_methods` | `crates/lancet-native/src/server/native_registry.rs:299` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `registry_full_catalog_registered_via_production_helper` | `crates/lancet-native/src/server/registry.rs:131` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `policy_denial_precedes_adapter` | `crates/lancet-native/src/server/registry.rs:310` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `path_denial_records_preflight_only` | `crates/lattice-api/tests/upper_routing.rs:316` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `backend_failure_records_both_invocations` | `crates/lancet-native/src/server/registry.rs:339` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `verification_failure_after_backend` | `crates/lancet-native/src/server/registry.rs:354` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `success_has_no_rejection_fields` | `crates/lancet-native/src/server/registry.rs:369` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `null_backend_records_backend_identity` | `crates/lancet-native/src/server/registry.rs:379` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `invocation_flags_are_monotonic` | `crates/lancet-native/src/server/registry.rs:397` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `cep_warn_harness` | `crates/lancet-native/tests/cep_warn_harness.rs:405` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `integration_read_file_allowed` | `crates/lancet-native/tests/policy_integration.rs:53` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `integration_web_fetch_denied_by_network_policy` | `crates/lancet-native/tests/policy_integration.rs:70` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `integration_web_fetch_allowed_for_allowed_host` | `crates/lancet-native/tests/policy_integration.rs:89` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `integration_budget_exceeded_denies_observation` | `crates/lancet-native/tests/policy_integration.rs:104` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `integration_todo_budget_exceeded_denies_todo_write` | `crates/lancet-native/tests/policy_integration.rs:130` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `read_only_turn_projection_is_isomorphic` | `crates/lancet-native/tests/prov_isomorphism.rs:58` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `mutation_turn_projection_is_isomorphic` | `crates/lancet-native/tests/prov_isomorphism.rs:86` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |
| `mixed_turn_projection_is_isomorphic` | `crates/lancet-native/tests/prov_isomorphism.rs:121` | 保留 | 局部不变量 | — | native handler 路径/scope/verify/edit/shell/fetch/effect-contract 局部安全契约；保留 |

### `lancet-protocol`（保留 36 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `unknown_impact_kind_fails_deserialization` | `crates/lancet-protocol/src/effect_types.rs:155` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `every_family_has_code_in_range` | `crates/lancet-protocol/src/error.rs:356` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `family_code_round_trips_through_jsonrpc_error_response` | `crates/lancet-protocol/src/error.rs:378` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `cep_error_data_is_tagged_and_rejects_free_form_json` | `crates/lancet-protocol/src/error.rs:468` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `retriability_present_after_round_trip` | `crates/lancet-protocol/src/error.rs:483` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `method_not_found_yields_32601` | `crates/lancet-protocol/src/error.rs:500` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `standard_jsonrpc_helpers_share_wire_mapping` | `crates/lancet-protocol/src/error.rs:507` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `no_1000_series_codes_in_module` | `crates/lancet-protocol/src/error.rs:536` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `typed_data_not_raw_value` | `crates/lancet-protocol/src/error.rs:567` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `effect_error_data_carries_rich_violation_evidence` | `crates/lancet-protocol/src/error.rs:578` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `tool_call_is_jsonrpc_request` | `crates/lancet-protocol/src/frame.rs:29` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `tool_event_is_jsonrpc_notification` | `crates/lancet-protocol/src/frame.rs:42` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `tool_result_success` | `crates/lancet-protocol/src/frame.rs:52` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `tool_error_is_error_object_with_cep_data` | `crates/lancet-protocol/src/frame.rs:63` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `request_round_trip_with_object_params` | `crates/lancet-protocol/src/jsonrpc.rs:160` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `notification_serializes_without_id` | `crates/lancet-protocol/src/jsonrpc.rs:177` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `success_response_round_trip` | `crates/lancet-protocol/src/jsonrpc.rs:189` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `error_response_round_trip` | `crates/lancet-protocol/src/jsonrpc.rs:201` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `response_with_both_result_and_error_rejected` | `crates/lancet-protocol/src/jsonrpc.rs:217` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `response_with_neither_result_nor_error_rejected` | `crates/lancet-protocol/src/jsonrpc.rs:230` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `params_are_direct_method_data` | `crates/lancet-protocol/src/jsonrpc.rs:241` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_decision_allowed_round_trips` | `crates/lancet-protocol/src/policy.rs:200` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_decision_denied_round_trips` | `crates/lancet-protocol/src/policy.rs:209` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_decision_denied_without_action_round_trips` | `crates/lancet-protocol/src/policy.rs:221` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_decision_human_review_round_trips` | `crates/lancet-protocol/src/policy.rs:233` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `unknown_policy_reason_code_fails_deserialization` | `crates/lancet-protocol/src/policy.rs:246` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `unknown_policy_action_required_fails_deserialization` | `crates/lancet-protocol/src/policy.rs:253` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_input_round_trips` | `crates/lancet-protocol/src/policy.rs:276` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_input_round_trips_with_empty_params` | `crates/lancet-protocol/src/policy.rs:289` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `budget_snapshot_embedded_in_audit_context_round_trips` | `crates/lancet-protocol/src/policy.rs:314` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_reason_code_has_expected_variants` | `crates/lancet-protocol/src/policy.rs:339` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_action_required_has_expected_variants` | `crates/lancet-protocol/src/policy.rs:361` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `policy_hook_is_object_safe` | `crates/lancet-protocol/src/policy.rs:380` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `prov_graph_fragment_round_trips` | `crates/lancet-protocol/src/provenance.rs:161` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `prov_activity_attributes_default_for_legacy_fragments` | `crates/lancet-protocol/src/provenance.rs:203` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |
| `in_toto_statement_deserializes_with_unknown_fields` | `crates/lancet-protocol/src/provenance.rs:218` | 保留 | 局部不变量 | — | lancet-protocol 局部不变量（wire/安全/dispatch/投影）；保留 |

### `lattice-agent`（保留 68 / 提升 3 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `append_llm_message_extends_provider_cache` | `crates/lattice-agent/src/agent_loop/messages.rs:58` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_ascii_short_noop` | `crates/lattice-agent/src/agent_loop/mod.rs:1346` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_ascii_long` | `crates/lattice-agent/src/agent_loop/mod.rs:1351` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_cjk_short_noop` | `crates/lattice-agent/src/agent_loop/mod.rs:1360` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_cjk_at_column_boundary` | `crates/lattice-agent/src/agent_loop/mod.rs:1366` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_mixed_ascii_cjk` | `crates/lattice-agent/src/agent_loop/mod.rs:1375` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_exact_width_noop` | `crates/lattice-agent/src/agent_loop/mod.rs:1383` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `truncate_empty` | `crates/lattice-agent/src/agent_loop/mod.rs:1389` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `agent_loop_builds_messages_from_transcript_and_user_input` | `crates/lattice-agent/tests/agent_loop.rs:114` | 提升 | Direct-Local 外部二进制 | `planned_direct_message_assembly` | 原内部测试断言集合由 #24 精确场景 `planned_direct_message_assembly` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `agent_loop_prepends_system_prompt_when_present` | `crates/lattice-agent/tests/agent_loop.rs:194` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `agent_loop_terminates_at_hard_cap_when_llm_loops_forever` | `crates/lattice-agent/tests/agent_loop.rs:282` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `agent_loop_second_llm_round_includes_tool_result_in_request` | `crates/lattice-agent/tests/agent_loop.rs:305` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `tool_execution_error_structured_payload_preserved` | `crates/lattice-agent/tests/agent_loop.rs:397` | 提升 | Direct-Local 外部二进制 | `planned_direct_tool_error_payload` | 原内部测试断言集合由 #24 精确场景 `planned_direct_tool_error_payload` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `scenario_01_cancel_before_tool_execution` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:258` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_06_cancel_after_first_tool_in_multi_tool_batch` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:355` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_02_cancel_during_tool_execution` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:483` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_03_live_sender_mirrors_result_events` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:599` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_04_receiver_drop_does_not_cancel_turn` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:667` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_04b_full_live_channel_without_drain_completes` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:734` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_05_cancel_mid_stream_preserves_partial_content` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:866` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_07_run_turn_buffered_matches_run_turn_live` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:934` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_08_timeout_cooperative_preserves_partial` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:1040` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_08b_timeout_on_stream_creation` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:1125` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_08c_timeout_during_tool_execution_preserves_state` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:1179` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `scenario_08d_expired_deadline_wins_over_ready_stream` | `crates/lattice-agent/tests/live_sink_and_cancel.rs:1270` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_result_has_cleanup_exhausted_field` | `crates/lattice-agent/tests/provisional_completion.rs:121` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_hook_complete_exhausted_true` | `crates/lattice-agent/tests/provisional_completion.rs:141` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_hook_error_becomes_terminal` | `crates/lattice-agent/tests/provisional_completion.rs:161` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_no_hook_behaves_like_run_turn_live` | `crates/lattice-agent/tests/provisional_completion.rs:188` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_hook_receives_assistant_message` | `crates/lattice-agent/tests/provisional_completion.rs:206` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_contract_no_workspace_dependency` | `crates/lattice-agent/tests/provisional_completion.rs:254` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_continue_preserves_message_ordering` | `crates/lattice-agent/tests/provisional_completion.rs:344` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_continue_accumulates_usage` | `crates/lattice-agent/tests/provisional_completion.rs:412` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_continue_increases_round_count` | `crates/lattice-agent/tests/provisional_completion.rs:467` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_continue_respects_cancellation` | `crates/lattice-agent/tests/provisional_completion.rs:496` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `provisional_complete_existing_run_turn_live_unaffected` | `crates/lattice-agent/tests/provisional_completion.rs:556` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_multi_chunk_tokens_assemble_into_one_message` | `crates/lattice-agent/tests/streaming_delta.rs:114` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_reasoning_separate_from_visible` | `crates/lattice-agent/tests/streaming_delta.rs:168` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_tool_call_assembly_and_sequential_execution` | `crates/lattice-agent/tests/streaming_delta.rs:210` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_incomplete_tool_call_error_no_execution` | `crates/lattice-agent/tests/streaming_delta.rs:299` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_invalid_tool_call_json_no_execution` | `crates/lattice-agent/tests/streaming_delta.rs:353` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_truncated_tool_call_json_reports_finish_reason_length` | `crates/lattice-agent/tests/streaming_delta.rs:438` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_tool_call_delta_after_end_is_error_no_execution` | `crates/lattice-agent/tests/streaming_delta.rs:479` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_duplicate_tool_call_end_is_error_no_execution` | `crates/lattice-agent/tests/streaming_delta.rs:546` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_multiple_tool_calls_all_valid_sequential` | `crates/lattice-agent/tests/streaming_delta.rs:616` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_multiple_tool_calls_one_invalid_none_executed` | `crates/lattice-agent/tests/streaming_delta.rs:685` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_mid_stream_error_preserves_partial_content` | `crates/lattice-agent/tests/streaming_delta.rs:741` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_exhausted_without_done_is_error` | `crates/lattice-agent/tests/streaming_delta.rs:802` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_failed_tool_does_not_make_turn_error` | `crates/lattice-agent/tests/streaming_delta.rs:855` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_multi_round_tool_loop` | `crates/lattice-agent/tests/streaming_delta.rs:920` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_started_event_ignored_no_double_model_started` | `crates/lattice-agent/tests/streaming_delta.rs:1002` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_backward_compat_fake_llm_chat_only_still_works` | `crates/lattice-agent/tests/streaming_delta.rs:1048` | 提升 | Direct-Local 外部二进制 | `planned_direct_streaming_chat_compat` | 原内部测试断言集合由 #24 精确场景 `planned_direct_streaming_chat_compat` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `streaming_open_retries_retryable_transport_error_then_succeeds` | `crates/lattice-agent/tests/streaming_delta.rs:1077` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_open_retry_exhaustion_returns_structured_error` | `crates/lattice-agent/tests/streaming_delta.rs:1152` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `streaming_open_does_not_retry_non_retryable_provider_http_error` | `crates/lattice-agent/tests/streaming_delta.rs:1202` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_content_only_emits_token_done` | `crates/lattice-agent/tests/streaming_seam.rs:65` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_with_reasoning_emits_reasoning_token_done` | `crates/lattice-agent/tests/streaming_seam.rs:78` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_with_tool_call_emits_start_delta_end_done` | `crates/lattice-agent/tests/streaming_seam.rs:100` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_skips_tool_call_delta_when_arguments_empty` | `crates/lattice-agent/tests/streaming_seam.rs:136` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_preserves_finish_reason_and_usage` | `crates/lattice-agent/tests/streaming_seam.rs:173` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_returns_err_when_chat_fails` | `crates/lattice-agent/tests/streaming_seam.rs:202` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_streaming_yields_successive_sequences_across_calls` | `crates/lattice-agent/tests/streaming_seam.rs:216` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_stream_chat_returns_err_for_queued_startup_error` | `crates/lattice-agent/tests/streaming_seam.rs:241` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_stream_chat_yields_llm_error_event_for_mid_stream_error` | `crates/lattice-agent/tests/streaming_seam.rs:256` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_chat_aggregates_queued_events_to_chat_response` | `crates/lattice-agent/tests/streaming_seam.rs:279` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_chat_ignores_provider_metadata_events` | `crates/lattice-agent/tests/streaming_seam.rs:325` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_with_multiple_tool_calls_emits_all_sequentially` | `crates/lattice-agent/tests/streaming_seam.rs:358` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_with_content_and_tool_calls_emits_token_before_tools` | `crates/lattice-agent/tests/streaming_seam.rs:419` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `default_stream_chat_with_empty_response_emits_only_done` | `crates/lattice-agent/tests/streaming_seam.rs:461` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_chat_returns_err_when_queue_exhausted` | `crates/lattice-agent/tests/streaming_seam.rs:473` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |
| `fake_stream_chat_returns_err_when_queue_exhausted` | `crates/lattice-agent/tests/streaming_seam.rs:486` | 保留 | 局部不变量 | — | AgentLoop 状态机/消息装配/可注入 trait 局部契约；保留 |

### `lattice-api`（保留 223 / 提升 25 / 删除 12）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `create_and_open_session_round_trip` | `crates/lattice-api/src/session.rs:389` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `status_reports_head_branch_and_commit_count` | `crates/lattice-api/src/session.rs:416` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `list_sessions_includes_created_sessions` | `crates/lattice-api/src/session.rs:431` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `session_id_is_32_char_hex` | `crates/lattice-api/src/session.rs:454` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `prefix_resolve_unique_match` | `crates/lattice-api/src/session.rs:468` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `prefix_resolve_ambiguous_reports_error` | `crates/lattice-api/src/session.rs:483` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `empty_prefix_rejected` | `crates/lattice-api/src/session.rs:497` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `short_prefix_rejected` | `crates/lattice-api/src/session.rs:511` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `log_returns_turn_entries` | `crates/lattice-api/src/session.rs:523` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `checkout_changes_head_state` | `crates/lattice-api/src/session.rs:534` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `branch_creates_pointer_to_commit` | `crates/lattice-api/src/session.rs:558` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `diff_reports_unique_commits_on_each_side` | `crates/lattice-api/src/session.rs:571` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `reset_moves_branch_and_returns_old_head` | `crates/lattice-api/src/session.rs:614` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `session_api_does_not_expose_legacy_operations` | `crates/lattice-api/src/session.rs:626` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `graph_store_trait_excludes_legacy_methods` | `crates/lattice-api/src/session.rs:654` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `session_api_rejects_empty_project_branch` | `crates/lattice-api/src/session.rs:662` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `session_api_rejects_whitespace_project_branch` | `crates/lattice-api/src/session.rs:679` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `create_session_persists_explicit_project_branch` | `crates/lattice-api/src/session.rs:696` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_context_profile_policy_defaults_when_cep_absent` | `crates/lattice-api/src/turn/cep.rs:746` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `cep_context_profile_policy_canonicalizes_observation_roots` | `crates/lattice-api/src/turn/cep.rs:758` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `cep_context_profile_policy_rejects_roots_outside_project` | `crates/lattice-api/src/turn/cep.rs:777` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `cep_context_profile_policy_network_max_web_response_bytes` | `crates/lattice-api/src/turn/cep.rs:792` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `workspace_policy_from_profile_lowers_denied_write_patterns` | `crates/lattice-api/src/turn/cep.rs:806` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `workspace_policy_from_profile_rejects_traversal_patterns` | `crates/lattice-api/src/turn/cep.rs:819` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `workspace_policy_from_profile_absent_defaults_empty` | `crates/lattice-api/src/turn/cep.rs:836` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `workspace_policy_applies_denied_patterns_to_mutate_region_in_policy_config` | `crates/lattice-api/src/turn/cep.rs:843` | 保留 | 局部不变量 | — | profile→policy 翻译纯映射局部契约；保留 |
| `external_audit_preserves_lower` | `crates/lattice-api/src/turn/cep.rs:961` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_lifecycle_session_tool_creates_lease` | `crates/lattice-api/src/turn/cep.rs:1042` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `catalog_exposure_omits_unknown_envelope_method` | `crates/lattice-api/src/turn/cep.rs:1075` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_retired_methods_return_method_not_found` | `crates/lattice-api/src/turn/cep_tests.rs:61` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_parse_error_maps_to_32700` | `crates/lattice-api/src/turn/cep_tests.rs:85` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_allows_enabled_workspace_read_file` | `crates/lattice-api/src/turn/cep_tests.rs:108` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_unknown_method_maps_to_32601` | `crates/lattice-api/src/turn/cep_tests.rs:145` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_invalid_params_maps_to_32602` | `crates/lattice-api/src/turn/cep_tests.rs:163` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_executor_adapter_validation_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:240` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `cep_executor_adapter_permission_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:250` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `cep_executor_adapter_execution_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:260` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `cep_executor_adapter_state_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:271` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `cep_executor_adapter_guard_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:281` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `cep_executor_adapter_effect_family_preserves_payload` | `crates/lattice-api/src/turn/cep_tests.rs:291` | 保留 | 局部不变量 | - | lattice-api 局部契约（CEP error family 负载保持）；保留 |
| `turn_runtime_constructs_one_adapter_per_turn` | `crates/lattice-api/src/turn/cep_tests.rs:307` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_native_registry_production_runtime_uses_native_audit_stage` | `crates/lattice-api/src/turn/cep_tests.rs:354` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `attestation_matches_replay_statement` | `crates/lattice-api/src/turn/cep_tests.rs:414` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `replay_without_stored_attestation` | `crates/lattice-api/src/turn/cep_tests.rs:485` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `completed_content_only` | `crates/lattice-api/src/turn/collector.rs:209` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `multi_round_tool_loop_uses_final_stop_tokens` | `crates/lattice-api/src/turn/collector.rs:253` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `tool_call_round_content_preserved_when_turn_ends_on_tool_calls` | `crates/lattice-api/src/turn/collector.rs:314` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `error_after_partial_tokens` | `crates/lattice-api/src/turn/collector.rs:360` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `cancelled_keeps_partial_output` | `crates/lattice-api/src/turn/collector.rs:388` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `commit_failed_returns_error` | `crates/lattice-api/src/turn/collector.rs:416` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `missing_turn_committed_returns_error` | `crates/lattice-api/src/turn/collector.rs:433` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_collector_captures_promoted` | `crates/lattice-api/src/turn/collector.rs:447` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_independence_same_status` | `crates/lattice-api/src/turn/collector.rs:473` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `disposition_parity_matrix_status_x_disposition` | `crates/lattice-api/src/turn/collector.rs:516` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `collector_no_disposition_inference_from_status` | `crates/lattice-api/src/turn/collector.rs:575` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `cep_ordered_emitter_defers_tool_finish_until_late_cep_lifecycle` | `crates/lattice-api/src/turn/live.rs:503` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `cep_ordered_emitter_releases_deferred_tool_finish_before_next_plain_event` | `crates/lattice-api/src/turn/live.rs:529` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `live_activation_completed_classifies_as_workspace_less` | `crates/lattice-api/src/turn/live.rs:580` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `live_activation_error_classifies_as_aborted` | `crates/lattice-api/src/turn/live.rs:596` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `live_activation_cancelled_classifies_as_aborted` | `crates/lattice-api/src/turn/live.rs:612` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `live_activation_outcome_messages_are_typed` | `crates/lattice-api/src/turn/live.rs:628` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `live_activation_no_candidate_exported_flag` | `crates/lattice-api/src/turn/live.rs:647` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `stream_turn_live_with_agent_basic_completed` | `crates/lattice-api/src/turn/live_tests.rs:273` | 提升 | Direct-Local 外部二进制 | `planned_direct_basic_completed_event_contract` | 原内部测试断言集合由 #24 精确场景 `planned_direct_basic_completed_event_contract` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_error_commits_error` | `crates/lattice-api/src/turn/live_tests.rs:364` | 提升 | Direct-Local 外部二进制 | `planned_direct_live_error_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_live_error_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_event_ordering` | `crates/lattice-api/src/turn/live_tests.rs:421` | 提升 | Direct-Local 外部二进制 | `planned_direct_live_event_order` | 原内部测试断言集合由 #24 精确场景 `planned_direct_live_event_order` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_startup_failure_no_stream` | `crates/lattice-api/src/turn/live_tests.rs:492` | 提升 | Direct-Local 外部二进制 | `planned_direct_provider_startup_failure` | 原内部测试断言集合由 #24 精确场景 `planned_direct_provider_startup_failure` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_commit_stores_delta_only` | `crates/lattice-api/src/turn/live_tests.rs:513` | 提升 | Direct-Local 外部二进制 | `planned_direct_delta_only_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_delta_only_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_branch_populated` | `crates/lattice-api/src/turn/live_tests.rs:644` | 提升 | Direct-Local 外部二进制 | `planned_direct_branch_population` | 原内部测试断言集合由 #24 精确场景 `planned_direct_branch_population` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_commit_failure_deterministic` | `crates/lattice-api/src/turn/live_tests.rs:675` | 提升 | Direct-Local 外部二进制 | `planned_direct_commit_failure` | 原内部测试断言集合由 #24 精确场景 `planned_direct_commit_failure` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_receiver_drop_does_not_cancel` | `crates/lattice-api/src/turn/live_tests.rs:836` | 提升 | Direct-Local 外部二进制 | `planned_direct_receiver_drop_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_receiver_drop_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_live_with_agent_slow_observer_still_commits` | `crates/lattice-api/src/turn/live_tests.rs:896` | 提升 | Direct-Local 外部二进制 | `planned_direct_slow_observer_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_slow_observer_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `run_turn_uses_required_session_and_returns_session_id` | `crates/lattice-api/src/turn/run_tests.rs:17` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_commits_exactly_one_turn` | `crates/lattice-api/src/turn/run_tests.rs:43` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_uses_existing_session_transcript` | `crates/lattice-api/src/turn/run_tests.rs:62` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_result_contains_contract_fields` | `crates/lattice-api/src/turn/run_tests.rs:91` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_failed_outcome_is_committed` | `crates/lattice-api/src/turn/run_tests.rs:129` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_cancelled_outcome_is_committed` | `crates/lattice-api/src/turn/run_tests.rs:160` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_cutover_uses_loaded_profile_model` | `crates/lattice-api/src/turn/run_tests.rs:214` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_plumbs_stable_provider_to_agent_request_and_commit_metadata` | `crates/lattice-api/src/turn/run_tests.rs:240` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `run_turn_commits_parent_from_initial_transcript` | `crates/lattice-api/src/turn/run_tests.rs:269` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `stream_turn_emits_tool_activity_before_committed` | `crates/lattice-api/src/turn/stream_tests.rs:17` | 提升 | Direct-Local 外部二进制 | `planned_direct_tool_activity_before_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_tool_activity_before_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_error_still_commits_error` | `crates/lattice-api/src/turn/stream_tests.rs:89` | 提升 | Direct-Local 外部二进制 | `planned_direct_stream_error_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_stream_error_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_cancelled_commits_cancelled` | `crates/lattice-api/src/turn/stream_tests.rs:142` | 提升 | Direct-Local 外部二进制 | `planned_direct_cancelled_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_cancelled_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_error_after_partial_commits_error` | `crates/lattice-api/src/turn/stream_tests.rs:217` | 提升 | Direct-Local 外部二进制 | `planned_direct_partial_output_error_commit` | 原内部测试断言集合由 #24 精确场景 `planned_direct_partial_output_error_commit` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_emits_cep_lifecycle_events` | `crates/lattice-api/src/turn/stream_tests.rs:282` | 提升 | Direct-Local 外部二进制 | `planned_direct_cep_lifecycle` | 原内部测试断言集合由 #24 精确场景 `planned_direct_cep_lifecycle` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `turn_usage_summary_from_turn_usage` | `crates/lattice-api/src/turn/types_tests.rs:8` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `status_error_maps_to_commit_error` | `crates/lattice-api/src/turn/types_tests.rs:21` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `turn_event_serializes_with_type_discriminator` | `crates/lattice-api/src/turn/types_tests.rs:29` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_unit_variant_serializes_correctly` | `crates/lattice-api/src/turn/types_tests.rs:39` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_struct_variant_with_optional_none` | `crates/lattice-api/src/turn/types_tests.rs:54` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_llm_request_error_serializes_structured_fields` | `crates/lattice-api/src/turn/types_tests.rs:66` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_turn_started_serializes` | `crates/lattice-api/src/turn/types_tests.rs:95` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `old_failed_variant_removed_compile_guard` | `crates/lattice-api/src/turn/types_tests.rs:107` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `turn_event_disposition_turn_committed_has_status_and_disposition` | `crates/lattice-api/src/turn/types_tests.rs:116` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_all_variants_roundtrip` | `crates/lattice-api/src/turn/types_tests.rs:139` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_run_turn_result_has_disposition` | `crates/lattice-api/src/turn/types_tests.rs:165` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_missing_fails_deserialization` | `crates/lattice-api/src/turn/types_tests.rs:192` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `turn_event_disposition_unknown_variant_fails` | `crates/lattice-api/src/turn/types_tests.rs:209` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `upper_workspace_source_missing_image_fails` | `crates/lattice-api/src/turn/upper.rs:333` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_workspace_source_explicit_oci_selection` | `crates/lattice-api/src/turn/upper.rs:340` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_workspace_source_explicit_template_selection` | `crates/lattice-api/src/turn/upper.rs:347` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_workspace_source_invalid_ambiguous_source_rejected` | `crates/lattice-api/src/turn/upper.rs:354` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_config_baseline_oid_matches_head` | `crates/lattice-api/src/turn/upper.rs:388` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_config_default_lifetime_constant` | `crates/lattice-api/src/turn/upper.rs:395` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_config_resolves_with_explicit_image` | `crates/lattice-api/src/turn/upper.rs:400` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `upper_lease_destroy_consumes_handle_once` | `crates/lattice-api/src/turn/upper.rs:516` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `workspace_execution_backend_changed_paths_from_typed_entries` | `crates/lattice-api/src/turn/workspace_backend.rs:447` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `workspace_execution_backend_maps_content_too_large` | `crates/lattice-api/src/turn/workspace_backend.rs:482` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `full_turn_audit_rows_and_replay_fragment` | `crates/lattice-api/tests/audit_turn_provenance.rs:145` | 提升 | Direct-Local 外部二进制 | `planned_direct_turn_audit_replay` | 原内部测试断言集合由 #24 精确场景 `planned_direct_turn_audit_replay` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `cep_mode_enforce_effect_overreach_denied` | `crates/lattice-api/tests/cep_mode_integration.rs:100` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_mode_warn_effect_overreach_ok_with_audit_warning` | `crates/lattice-api/tests/cep_mode_integration.rs:143` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `effect_deny_rejects_in_warn_mode` | `crates/lattice-api/tests/cep_mode_integration.rs:169` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_mode_off_effect_overreach_passes` | `crates/lattice-api/tests/cep_mode_integration.rs:196` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `effect_deny_rejects_in_off_mode` | `crates/lattice-api/tests/cep_mode_integration.rs:214` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_mode_off_policy_deny_still_enforced` | `crates/lattice-api/tests/cep_mode_integration.rs:244` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_mode_default_enforce_from_profile_schema` | `crates/lattice-api/tests/cep_mode_integration.rs:316` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_mode_enforce_when_cep_section_mode_absent` | `crates/lattice-api/tests/cep_mode_integration.rs:331` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_round_trip_impact_matrix` | `crates/lattice-api/tests/cep_round_trip.rs:228` | 保留 | 局部不变量 | — | CEP impact 矩阵局部契约；保留 |
| `cep_round_trip_unknown_method_rejected_at_adapter` | `crates/lattice-api/tests/cep_round_trip.rs:311` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cep_round_trip_schema_mismatch_preserves_field_path` | `crates/lattice-api/tests/cep_round_trip.rs:335` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `cep_round_trip_cep_family_error_preserves_payload` | `crates/lattice-api/tests/cep_round_trip.rs:365` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `web_fetch_and_todo_write_audit_without_workspace_envelope` | `crates/lattice-api/tests/cep_round_trip.rs:488` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `consistency_completed_content_only` | `crates/lattice-api/tests/consistency.rs:274` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `consistency_error_after_partial_token` | `crates/lattice-api/tests/consistency.rs:294` | 提升 | Direct-Local 外部二进制 | `planned_direct_partial_token_parity` | 原内部测试断言集合由 #24 精确场景 `planned_direct_partial_token_parity` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `consistency_tool_call_path` | `crates/lattice-api/tests/consistency.rs:357` | 提升 | Direct-Local 外部二进制 | `planned_direct_tool_call_parity` | 原内部测试断言集合由 #24 精确场景 `planned_direct_tool_call_parity` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `stream_turn_emits_activity_before_commit` | `crates/lattice-api/tests/consistency.rs:438` | 提升 | Direct-Local 外部二进制 | `planned_direct_activity_commit_order` | 原内部测试断言集合由 #24 精确场景 `planned_direct_activity_commit_order` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `consistency_completed_disposition_parity` | `crates/lattice-api/tests/consistency.rs:492` | 提升 | Direct-Local 外部二进制 | `planned_direct_completed_disposition_parity` | 原内部测试断言集合由 #24 精确场景 `planned_direct_completed_disposition_parity` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `consistency_error_disposition_parity` | `crates/lattice-api/tests/consistency.rs:566` | 提升 | Direct-Local 外部二进制 | `planned_direct_error_disposition_parity` | 原内部测试断言集合由 #24 精确场景 `planned_direct_error_disposition_parity` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `dogfood_impact_matrix_five_production_classes` | `crates/lattice-api/tests/dogfood_impact_matrix.rs:248` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `git_store_branch_and_checkout_moves_head` | `crates/lattice-api/tests/git_store_branch_checkout.rs:46` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_candidate_ref_created_for_lifecycle_turn` | `crates/lattice-api/tests/git_store_candidate_refs.rs:24` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_concurrent_commit_same_parent_second_conflicts` | `crates/lattice-api/tests/git_store_concurrent_commit_safety.rs:54` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_threaded_concurrent_commits_one_wins_one_conflicts` | `crates/lattice-api/tests/git_store_concurrent_commit_safety.rs:117` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_diff_reports_unique_commits_per_side` | `crates/lattice-api/tests/git_store_diff.rs:47` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_head_transcript_reads_ordered_messages` | `crates/lattice-api/tests/git_store_head_transcript.rs:48` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_no_remote_push_refspec_for_lattice_refs` | `crates/lattice-api/tests/git_store_no_remote_refspec.rs:42` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_get_turn_commit_includes_profile_snapshot_json` | `crates/lattice-api/tests/git_store_profile_snapshot.rs:25` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_reset_moves_branch_pointer` | `crates/lattice-api/tests/git_store_reset.rs:46` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_create_and_reopen_preserves_identity` | `crates/lattice-api/tests/git_store_session_create_open.rs:21` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_session_id_prefix_resolution` | `crates/lattice-api/tests/git_store_session_prefix_resolve.rs:20` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_snapshot_lost_flag_round_trips` | `crates/lattice-api/tests/git_store_snapshot_lost.rs:24` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `git_store_turn_commit_creates_git_history` | `crates/lattice-api/tests/git_store_turn_commit_history.rs:25` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `dirty_managed_lower_blocks_preflight` | `crates/lattice-api/tests/promotion_wiring.rs:257` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_journal_returns_recovery_required` | `crates/lattice-api/tests/promotion_wiring.rs:271` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `third_state_journal_returns_recovery_conflict` | `crates/lattice-api/tests/promotion_wiring.rs:287` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `recover_enables_preflight_and_promotion` | `crates/lattice-api/tests/promotion_wiring.rs:309` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `stale_verification_reaches_no_promotion_effects` | `crates/lattice-api/tests/promotion_wiring.rs:329` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `success_uses_real_session_callback_and_materializes_lower` | `crates/lattice-api/tests/promotion_wiring.rs:381` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `public_api_compiles_for_embedder` | `crates/lattice-api/tests/promotion_wiring.rs:470` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `promotion_source_contains_no_export_candidate_call` | `crates/lattice-api/tests/promotion_wiring.rs:500` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `preexported_promotion_succeeds_without_workspace` | `crates/lattice-api/tests/promotion_wiring.rs:512` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `preexported_promotion_passes_verification_evidence_to_callback` | `crates/lattice-api/tests/promotion_wiring.rs:557` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `preexported_stale_verification_is_rejected` | `crates/lattice-api/tests/promotion_wiring.rs:615` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `conflict_is_distinct_from_other_variants` | `crates/lattice-api/tests/promotion_wiring.rs:666` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `baseline_conflict_classifies_as_conflict` | `crates/lattice-api/tests/promotion_wiring.rs:680` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `lower_dirty_classifies_as_temporarily_blocked` | `crates/lattice-api/tests/promotion_wiring.rs:693` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `verification_mismatch_classifies_as_rejected` | `crates/lattice-api/tests/promotion_wiring.rs:704` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `recovery_required_classifies_correctly` | `crates/lattice-api/tests/promotion_wiring.rs:717` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `infrastructure_failure_classifies_as_internal` | `crates/lattice-api/tests/promotion_wiring.rs:727` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `temporarily_blocked_pins_candidate_before_returning` | `crates/lattice-api/tests/promotion_wiring.rs:734` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `retry_pinned_candidate_succeeds_after_temporary_block` | `crates/lattice-api/tests/promotion_wiring.rs:799` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `stream_turn_sources_do_not_reference_promotion_apis` | `crates/lattice-api/tests/promotion_wiring.rs:848` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `production_code_does_not_fabricate_passing_verification` | `crates/lattice-api/tests/promotion_wiring.rs:866` | 删除 | 局部不变量 | n/a — 无替代场景（架构边界由 xtask/harness-boundary 负责） | 扫描源码 import/crate 名/manifest 字符串；删除（RFC #19 决定六） |
| `promotion_module_documents_activation_requirement` | `crates/lattice-api/tests/promotion_wiring.rs:893` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `deterministic_embedder_path_remains_primary` | `crates/lattice-api/tests/promotion_wiring.rs:900` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `microsandbox_workspace_type_checks_with_public_promotion_api` | `crates/lattice-api/tests/promotion_wiring.rs:911` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `release_e2e_criterion_8` | `crates/lattice-api/tests/release_e2e.rs:177` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `pending_worktree_with_snapshot_resolves_restore_snapshot` | `crates/lattice-api/tests/resume_tests.rs:107` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_verification_without_loss_resolves_restore_snapshot` | `crates/lattice-api/tests/resume_tests.rs:124` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_promotion_with_candidate_resolves_retry_promotion` | `crates/lattice-api/tests/resume_tests.rs:141` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_worktree_without_snapshot_fails_snapshot_required` | `crates/lattice-api/tests/resume_tests.rs:160` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_worktree_no_lifecycle_fails_snapshot_required` | `crates/lattice-api/tests/resume_tests.rs:171` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_promotion_without_candidate_fails_candidate_required` | `crates/lattice-api/tests/resume_tests.rs:178` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_promotion_no_lifecycle_fails_candidate_required` | `crates/lattice-api/tests/resume_tests.rs:189` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_conflict_rejects` | `crates/lattice-api/tests/resume_tests.rs:196` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `promoted_rejects` | `crates/lattice-api/tests/resume_tests.rs:206` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `rejected_rejects` | `crates/lattice-api/tests/resume_tests.rs:216` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `aborted_rejects` | `crates/lattice-api/tests/resume_tests.rs:226` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `not_applicable_rejects` | `crates/lattice-api/tests/resume_tests.rs:236` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `resume_with_nonexistent_branch_fails_target_resolution` | `crates/lattice-api/tests/resume_tests.rs:302` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `resume_without_project_branch_metadata_fails` | `crates/lattice-api/tests/resume_tests.rs:323` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `new_resume_commit_stores_exact_resumes_turn_oid` | `crates/lattice-api/tests/resume_tests.rs:354` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `pending_promotion_retry_invokes_no_snapshot_method` | `crates/lattice-api/tests/resume_tests.rs:376` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `prune_one_session_does_not_remove_another` | `crates/lattice-api/tests/resume_tests.rs:477` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `prune_idempotent_second_call_returns_zero_released` | `crates/lattice-api/tests/resume_tests.rs:522` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `prune_session_snapshots_skips_pending_worktree_snapshot` | `crates/lattice-api/tests/resume_tests.rs:557` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `journal_create_writes_header_and_is_parseable` | `crates/lattice-api/tests/snapshot_journal_tests.rs:120` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_append_intent_retained_committed` | `crates/lattice-api/tests/snapshot_journal_tests.rs:137` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_torn_final_line_is_tolerated` | `crates/lattice-api/tests/snapshot_journal_tests.rs:158` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_durable_remove_deletes_file` | `crates/lattice-api/tests/snapshot_journal_tests.rs:177` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_exists_returns_false_when_absent` | `crates/lattice-api/tests/snapshot_journal_tests.rs:192` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_exists_returns_true_after_create` | `crates/lattice-api/tests/snapshot_journal_tests.rs:198` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_fault_after_intent_converges` | `crates/lattice-api/tests/snapshot_journal_tests.rs:207` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_fault_after_retained_converges` | `crates/lattice-api/tests/snapshot_journal_tests.rs:226` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_fault_after_committed_converges` | `crates/lattice-api/tests/snapshot_journal_tests.rs:247` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_orphan_snapshot_is_released` | `crates/lattice-api/tests/snapshot_journal_tests.rs:273` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_phantom_snapshot_marks_lost` | `crates/lattice-api/tests/snapshot_journal_tests.rs:296` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_stale_completed_journal_is_cleared` | `crates/lattice-api/tests/snapshot_journal_tests.rs:322` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_reconcile_empty_journal_dir_succeeds` | `crates/lattice-api/tests/snapshot_journal_tests.rs:349` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_intent_only_orphan_releases_existing_snapshot` | `crates/lattice-api/tests/snapshot_journal_tests.rs:359` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_multiple_entries_reconcile_independently` | `crates/lattice-api/tests/snapshot_journal_tests.rs:378` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_parser_rejects_empty_file` | `crates/lattice-api/tests/snapshot_journal_tests.rs:413` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `journal_parser_rejects_missing_header` | `crates/lattice-api/tests/snapshot_journal_tests.rs:423` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_pending_worktree_retains_snapshot` | `crates/lattice-api/tests/snapshot_journal_tests.rs:436` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_pending_verification_retains_snapshot` | `crates/lattice-api/tests/snapshot_journal_tests.rs:471` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_pending_conflict_no_snapshot` | `crates/lattice-api/tests/snapshot_journal_tests.rs:506` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_pending_promotion_releases_after_pin` | `crates/lattice-api/tests/snapshot_journal_tests.rs:539` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_promoted_releases_snapshot` | `crates/lattice-api/tests/snapshot_journal_tests.rs:574` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_rejected_destroys` | `crates/lattice-api/tests/snapshot_journal_tests.rs:603` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_aborted_destroys` | `crates/lattice-api/tests/snapshot_journal_tests.rs:638` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_workspace_less_destroy` | `crates/lattice-api/tests/snapshot_journal_tests.rs:667` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_promoted_retry_first_failure_succeeds` | `crates/lattice-api/tests/snapshot_journal_tests.rs:691` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_promoted_retry_both_failures_returns_error` | `crates/lattice-api/tests/snapshot_journal_tests.rs:709` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_promoted_retry_uses_same_commit_id` | `crates/lattice-api/tests/snapshot_journal_tests.rs:725` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `resource_matrix_d14_retained_count_matches` | `crates/lattice-api/tests/snapshot_journal_tests.rs:749` | 保留 | 局部不变量 | — | session/git store 事务、恢复、原子性、ID 解析局部不变量；保留（非产品 turn 路径） |
| `cleanup_clean_returns_complete_immediately` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:84` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cleanup_dirty_appends_workspace_dirty_system_fact_and_continues` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:115` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `cleanup_exhaustion_yields_completed_pending_worktree` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:151` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `cleanup_status_error_yields_error_aborted` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:215` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `cleanup_same_loop_assertion_one_hook_covers_all_rounds` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:232` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `export_verify_promote_verification_executes_and_records_evidence` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:288` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `export_verify_promote_failed_verification_yields_pending` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:316` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `export_verify_promote_binding_assertion_oid_matches` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:354` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `export_verify_promote_count_assertion_exactly_one_export` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:395` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_workspace_less_completed_not_applicable` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:427` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_promoted_completed_promoted` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:438` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_cleanup_exhaustion_completed_pending_worktree` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:452` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_post_export_head_change_pending_verification` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:465` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_pinned_blocked_completed_pending_promotion` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:482` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_baseline_movement_completed_pending_conflict` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:498` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_export_rejected` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:511` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_validation_rejected` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:527` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_policy_rejected` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:543` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_agent_error_before_candidate_aborted` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:558` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_cancelled_before_candidate_aborted` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:572` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_independence_completed_pairs_with_all_dispositions` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:585` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_independence_error_cancelled_pair_with_aborted` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:646` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `outcome_matrix_retention_only_pending_worktree_and_verify_retain` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:667` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `workspace_dirty_fact_structured_json` | `crates/lattice-api/tests/turn_lifecycle_tests.rs:730` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `branch_source_missing_project_target_branch_fails` | `crates/lattice-api/tests/turn_target_tests.rs:100` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `branch_source_reads_session_project_target_branch` | `crates/lattice-api/tests/turn_target_tests.rs:145` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `branch_source_nonexistent_branch_fails` | `crates/lattice-api/tests/turn_target_tests.rs:182` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `branch_source_attached_branch_fails` | `crates/lattice-api/tests/turn_target_tests.rs:214` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `branch_source_graph_head_ignored` | `crates/lattice-api/tests/turn_target_tests.rs:241` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `exact_baseline_uses_injected_oid_not_head` | `crates/lattice-api/tests/turn_target_tests.rs:323` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `exact_baseline_none_falls_back_to_head` | `crates/lattice-api/tests/turn_target_tests.rs:352` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `exact_baseline_missing_image_fails` | `crates/lattice-api/tests/turn_target_tests.rs:368` | 保留 | 局部不变量 | — | lattice-api 局部契约（非成品端到端路径）；保留 |
| `observation_uses_upper_relative_paths` | `crates/lattice-api/tests/upper_routing.rs:250` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `mutations_preserve_lower_sentinel` | `crates/lattice-api/tests/upper_routing.rs:286` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `path_denial_records_preflight_only` | `crates/lattice-api/tests/upper_routing.rs:316` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `upper_routing_e2e` | `crates/lattice-api/tests/upper_routing.rs:375` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `adapter_failure_has_no_lower_fallback` | `crates/lattice-api/tests/upper_routing.rs:609` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `generated_profiles_are_executable_host_only` | `crates/lattice-api/tests/upper_routing.rs:699` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `microsandbox_upper_routing` | `crates/lattice-api/tests/upper_routing.rs:787` | 删除 | 局部不变量 | n/a | #37–#40 已删除该 Upper/promotion/MSB 产品契约；对应测试已不存在，不向 #24 虚构 replacement。 |
| `usage_run_turn_can_read_workspace_file_with_default_profile` | `crates/lattice-api/tests/usage_turn.rs:175` | 提升 | Direct-Local 外部二进制 | `planned_direct_workspace_read_turn` | 原内部测试断言集合由 #24 精确场景 `planned_direct_workspace_read_turn` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `usage_stream_turn_model_started_includes_routed_provider` | `crates/lattice-api/tests/usage_turn.rs:237` | 提升 | Direct-Local 外部二进制 | `planned_direct_routed_provider_event` | 原内部测试断言集合由 #24 精确场景 `planned_direct_routed_provider_event` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `usage_run_turn_can_edit_workspace_file_and_commit` | `crates/lattice-api/tests/usage_turn.rs:274` | 保留 | 局部不变量 | — | 公开 API 类型/错误/事件映射局部契约；保留 |
| `usage_resume_session_includes_previous_turn_context` | `crates/lattice-api/tests/usage_turn.rs:340` | 提升 | Direct-Local 外部二进制 | `planned_direct_reopen_previous_context` | 原内部测试断言集合由 #24 精确场景 `planned_direct_reopen_previous_context` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `public_https_policy_requires_fetch_capability` | `crates/lattice-api/tests/usage_turn.rs:454` | 提升 | Direct-Local 外部二进制 | `planned_direct_fetch_capability_denial` | 原内部测试断言集合由 #24 精确场景 `planned_direct_fetch_capability_denial` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `rejects_profile_disabled_web_fetch` | `crates/lattice-api/tests/usage_turn.rs:519` | 提升 | Direct-Local 外部二进制 | `planned_direct_disabled_fetch_denial` | 原内部测试断言集合由 #24 精确场景 `planned_direct_disabled_fetch_denial` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
### `lattice-cli`（保留 118 / 提升 57 / 删除 10）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `audit_query_prints_fragment_for_seeded_db` | `crates/lattice-cli/src/commands/audit.rs:351` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_query_seeded` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_query_seeded` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_tail_streams_rows_for_seeded_db` | `crates/lattice-cli/src/commands/audit.rs:361` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_tail_seeded` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_tail_seeded` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_query_errors_when_store_missing` | `crates/lattice-cli/src/commands/audit.rs:373` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_audit_cli` 接管 | 已删除原内部测试，由已落地 fast_audit_cli（audit query/replay/tail 空态/错误态 --json） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `audit_query_empty_result_for_unknown_session` | `crates/lattice-cli/src/commands/audit.rs:381` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_query_unknown_session` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_query_unknown_session` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_replay_reconstructs_turn_trace_for_seeded_db` | `crates/lattice-cli/src/commands/audit.rs:389` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_replay_trace` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_replay_trace` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_replay_reports_stored_attestation_match` | `crates/lattice-cli/src/commands/audit.rs:401` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_attestation_match` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_attestation_match` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_replay_errors_for_unknown_turn` | `crates/lattice-cli/src/commands/audit.rs:441` | 提升 | Direct-Local 外部二进制 | `planned_direct_audit_replay_unknown_turn` | 原内部测试断言集合由 #24 精确场景 `planned_direct_audit_replay_unknown_turn` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `audit_command_source_does_not_reference_cep_server` | `crates/lattice-cli/src/commands/audit.rs:458` | 删除 | 局部不变量 | n/a | 扫描源码禁止字符串；删除（RFC #19 决定六） |
| `audit_query_reads_legacy_schema_store` | `crates/lattice-cli/src/commands/audit.rs:481` | 保留 | 局部不变量 | - | #23 review-fix round-3 复审判偏差：手工植入旧 SQLite schema 测向后兼容；RFC #19 harness 边界禁止直接打开/植入内部 audit DB，#24 audit scenarios 永远无法覆盖（真实 turn 只生成当前 schema）；改记保留为局部不变量。 |
| `doctor_report_contains_expected_keys` | `crates/lattice-cli/src/commands/doctor.rs:162` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_json_output_has_expected_structure` | `crates/lattice-cli/src/commands/doctor.rs:172` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_text_output_includes_credential_env_var_names` | `crates/lattice-cli/src/commands/doctor.rs:187` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_text_output_includes_session_dir` | `crates/lattice-cli/src/commands/doctor.rs:197` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_text_output_includes_alias_count` | `crates/lattice-cli/src/commands/doctor.rs:207` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_json_does_not_leak_secret_values` | `crates/lattice-cli/src/commands/doctor.rs:216` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_text_does_not_leak_secret_values` | `crates/lattice-cli/src/commands/doctor.rs:229` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_credentials_reflect_present_status` | `crates/lattice-cli/src/commands/doctor.rs:242` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `doctor_credentials_reflect_missing_status` | `crates/lattice-cli/src/commands/doctor.rs:256` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `lancet_status_json_flag_parses` | `crates/lattice-cli/src/commands/lancet.rs:208` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `lancet_status_no_args_parses` | `crates/lattice-cli/src/commands/lancet.rs:218` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `lancet_doctor_parses` | `crates/lattice-cli/src/commands/lancet.rs:228` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cep_status_missing_db_returns_empty_state` | `crates/lattice-cli/src/commands/lancet.rs:288` | 提升 | 快速二进制黑盒 | 延期 `planned_lancet_cli` | 已删除原内部测试，由已落地 fast_lancet_cli（lancet status 空态 JSON/text）黑盒接管 |
| `cep_status_seeded_audit_json` | `crates/lattice-cli/src/commands/lancet.rs:297` | 提升 | Direct-Local 外部二进制 | `planned_direct_lancet_status_seeded` | 原内部测试断言集合由 #24 精确场景 `planned_direct_lancet_status_seeded` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `cep_doctor_passes_on_repo_root` | `crates/lattice-cli/src/commands/lancet.rs:345` | 保留 | 局部不变量 | - | #23 复审偏差：测试 run_doctor_inner_for_project_root（LANCET doctor catalog 方法数 + registry 注册检查）是编译期不变量检查，经 #[cfg(test)] helper 驱动，非公开黑盒行为；无已落地或 planned 场景能诚实接管，改记保留为局部不变量。 |
| `cli_diagnostics_models_auth_text_lists_aliases_one_per_line` | `crates/lattice-cli/src/commands/models.rs:37` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_models_auth_text_excludes_secret_values` | `crates/lattice-cli/src/commands/models.rs:45` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_models_auth_json_excludes_secrets` | `crates/lattice-cli/src/commands/models.rs:53` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_models_no_auth_lists_all_aliases` | `crates/lattice-cli/src/commands/models.rs:61` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_models_json_output_is_valid_json` | `crates/lattice-cli/src/commands/models.rs:67` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_default_text_includes_provider_and_model` | `crates/lattice-cli/src/commands/resolve.rs:59` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_verbose_includes_all_metadata` | `crates/lattice-cli/src/commands/resolve.rs:67` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_json_includes_all_fields` | `crates/lattice-cli/src/commands/resolve.rs:75` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_json_excludes_secret_values` | `crates/lattice-cli/src/commands/resolve.rs:85` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_unknown_model_returns_error` | `crates/lattice-cli/src/commands/resolve.rs:95` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_diagnostics_resolve_no_provider_override` | `crates/lattice-cli/src/commands/resolve.rs:109` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `stream_timeout_constant_is_5_minutes` | `crates/lattice-cli/src/commands/run_tests.rs:9` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `stream_timeout_fires_after_duration` | `crates/lattice-cli/src/commands/run_tests.rs:16` | 提升 | Direct-Local 外部二进制 | `planned_direct_provider_timeout` | 原内部测试断言集合由 #24 精确场景 `planned_direct_provider_timeout` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `run_validate_agents_dir_rejects_parent_components` | `crates/lattice-cli/src/commands/run_tests.rs:44` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `run_validate_agents_dir_resolves_symlink_prefix_before_safety_check` | `crates/lattice-cli/src/commands/run_tests.rs:78` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `run_default_session_dir_under_lattice_sessions` | `crates/lattice-cli/src/commands/run_tests.rs:98` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `run_integration_commits_existing_session` | `crates/lattice-cli/src/commands/run_tests.rs:148` | 提升 | Direct-Local 外部二进制 | `planned_direct_existing_session_run_result` | 原内部测试断言集合由 #24 精确场景 `planned_direct_existing_session_run_result` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `run_integration_missing_session_returns_not_found` | `crates/lattice-cli/src/commands/run_tests.rs:218` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_cli（session send <missing-id> 黑盒：open_session 在 workspace 前失败，exit 2 + stderr not found）接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `tool_executor_uses_workspace_root_not_profile_package_dir` | `crates/lattice-cli/src/commands/run_tests.rs:246` | 提升 | Direct-Local 外部二进制 | `planned_direct_canonical_workspace_root` | 原内部测试断言集合由 #24 精确场景 `planned_direct_canonical_workspace_root` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `run_source_has_no_old_crate_imports` | `crates/lattice-cli/src/commands/run_tests.rs:369` | 删除 | 局部不变量 | n/a | 扫描源码 import/crate 名/manifest 字符串；删除（RFC #19 决定六） |
| `render_json_event_emits_single_line_with_trailing_newline` | `crates/lattice-cli/src/commands/run_tests.rs:391` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_json_event_roundtrips_every_variant` | `crates/lattice-cli/src/commands/run_tests.rs:402` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_json_event_emits_no_ansi_escapes` | `crates/lattice-cli/src/commands/run_tests.rs:479` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_token_delta_writes_only_to_stdout` | `crates/lattice-cli/src/commands/run_tests.rs:494` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_status_line_after_partial_token_inserts_stderr_newline` | `crates/lattice-cli/src/commands/run_tests.rs:507` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `flush_before_status_resets_mid_token_flag_after_first_status` | `crates/lattice-cli/src/commands/run_tests.rs:549` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `render_human_tool_finished_ok_renders_byte_size` | `crates/lattice-cli/src/commands/run_tests.rs:585` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_tool_finished_err_renders_summary` | `crates/lattice-cli/src/commands/run_tests.rs:603` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_reasoning_hidden_default` | `crates/lattice-cli/src/commands/run_tests.rs:621` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_reasoning_visible_verbose` | `crates/lattice-cli/src/commands/run_tests.rs:636` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_turn_started_silent_default` | `crates/lattice-cli/src/commands/run_tests.rs:650` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `render_human_committed_lowercases_status` | `crates/lattice-cli/src/commands/run_tests.rs:667` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `verbose_epilogue_emits_all_fields_when_populated` | `crates/lattice-cli/src/commands/run_tests.rs:692` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `verbose_epilogue_skips_unset_fields` | `crates/lattice-cli/src/commands/run_tests.rs:720` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `verbose_epilogue_usage_from_state_not_committed` | `crates/lattice-cli/src/commands/run_tests.rs:731` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `exit_code_from_state_covers_all_da4_rows` | `crates/lattice-cli/src/commands/run_tests.rs:750` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `seq_happy_path_human` | `crates/lattice-cli/src/commands/run_tests.rs:863` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_happy_path_verbose_with_epilogue` | `crates/lattice-cli/src/commands/run_tests.rs:916` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_trailing_token_newline_emitted` | `crates/lattice-cli/src/commands/run_tests.rs:966` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_token_then_status_inserts_stderr_newline` | `crates/lattice-cli/src/commands/run_tests.rs:989` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_multiround_usage_sums_into_state` | `crates/lattice-cli/src/commands/run_tests.rs:1014` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `seq_tool_failure_renders_summary` | `crates/lattice-cli/src/commands/run_tests.rs:1055` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_turn_failed_then_commit_drains_stream` | `crates/lattice-cli/src/commands/run_tests.rs:1096` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_commit_failed_renders_reason` | `crates/lattice-cli/src/commands/run_tests.rs:1139` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_stream_ends_without_terminal_is_protocol_violation` | `crates/lattice-cli/src/commands/run_tests.rs:1162` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_jsonl_round_trips_every_variant` | `crates/lattice-cli/src/commands/run_tests.rs:1186` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_reasoning_hidden_default_in_human` | `crates/lattice-cli/src/commands/run_tests.rs:1297` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `seq_reasoning_visible_in_verbose` | `crates/lattice-cli/src/commands/run_tests.rs:1335` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `parse_status_no_args` | `crates/lattice-cli/src/commands/session.rs:922` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_status_with_session_id` | `crates/lattice-cli/src/commands/session.rs:932` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_list` | `crates/lattice-cli/src/commands/session.rs:944` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_log_with_session_id` | `crates/lattice-cli/src/commands/session.rs:951` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_log_without_session_id_fails` | `crates/lattice-cli/src/commands/session.rs:963` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `human_turn_log_renders_disposition_next_to_status` | `crates/lattice-cli/src/commands/session.rs:969` | 保留 | 局部不变量 | - | #23 review-fix round-4 复审判偏差：纯渲染器/序列化器局部不变量（合成 TurnEvent 直接驱动 render_human_event/render_json_event/fake_event_loop，非经公开 CLI）；direct_local_plain_text_turn_commits_and_exit_zero 只执行 plain-text 成功 turn + --json，不覆盖 human stdout/stderr 渲染、verbose/epilogue、工具/提交失败、协议错误、branch/reset/diff 作用域、缺凭据路径；改记保留为局部不变量。 |
| `parse_checkout_with_session_id_and_target` | `crates/lattice-cli/src/commands/session.rs:995` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_checkout_missing_target_fails` | `crates/lattice-cli/src/commands/session.rs:1008` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_checkout_missing_session_id_fails` | `crates/lattice-cli/src/commands/session.rs:1014` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_branch_list_mode` | `crates/lattice-cli/src/commands/session.rs:1020` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_branch_create_mode` | `crates/lattice-cli/src/commands/session.rs:1038` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_branch_without_session_id_fails` | `crates/lattice-cli/src/commands/session.rs:1056` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_diff` | `crates/lattice-cli/src/commands/session.rs:1062` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_diff_missing_args_fails` | `crates/lattice-cli/src/commands/session.rs:1080` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_diff_missing_session_id_fails` | `crates/lattice-cli/src/commands/session.rs:1086` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_reset` | `crates/lattice-cli/src/commands/session.rs:1092` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_reset_missing_args_fails` | `crates/lattice-cli/src/commands/session.rs:1105` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `parse_reset_missing_session_id_fails` | `crates/lattice-cli/src/commands/session.rs:1111` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_current` | `crates/lattice-cli/src/commands/session.rs:1117` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_show` | `crates/lattice-cli/src/commands/session.rs:1122` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_branches` | `crates/lattice-cli/src/commands/session.rs:1127` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_reflog` | `crates/lattice-cli/src/commands/session.rs:1132` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_tag` | `crates/lattice-cli/src/commands/session.rs:1137` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_tags` | `crates/lattice-cli/src/commands/session.rs:1142` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_cherry_pick` | `crates/lattice-cli/src/commands/session.rs:1147` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_merge` | `crates/lattice-cli/src/commands/session.rs:1152` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_rebase` | `crates/lattice-cli/src/commands/session.rs:1157` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_stash` | `crates/lattice-cli/src/commands/session.rs:1162` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_artifacts` | `crates/lattice-cli/src/commands/session.rs:1167` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_manifests` | `crates/lattice-cli/src/commands/session.rs:1172` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_gc` | `crates/lattice-cli/src/commands/session.rs:1177` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `reject_old_subcommand_rm` | `crates/lattice-cli/src/commands/session.rs:1182` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `migrate_subcommand_parses_with_flags` | `crates/lattice-cli/src/commands/session.rs:1187` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `not_found_error_includes_object_id` | `crates/lattice-cli/src/commands/session.rs:1220` | 删除 | 局部不变量 | n/a | 测试 #[cfg(test)] only helper not_found()；missing-id 行为已被 fast_session_cli 的 nonexistent-session 黑盒覆盖（status/log/send 均 assert not found + id in stderr）；删除。 |
| `summarize_short_text_unchanged` | `crates/lattice-cli/src/commands/session.rs:1228` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `summarize_long_text_truncated` | `crates/lattice-cli/src/commands/session.rs:1233` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `summarize_cjk_short_unchanged` | `crates/lattice-cli/src/commands/session.rs:1241` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `summarize_cjk_truncated_at_column_boundary` | `crates/lattice-cli/src/commands/session.rs:1247` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `summarize_mixed_ascii_cjk_truncated` | `crates/lattice-cli/src/commands/session.rs:1257` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `display_id_short_unchanged` | `crates/lattice-cli/src/commands/session.rs:1265` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `display_id_long_truncated` | `crates/lattice-cli/src/commands/session.rs:1270` | 保留 | 局部不变量 | - | #23 复审偏差：纯 CLI helper/常量/源码扫描行为，无已落地或 planned 场景能诚实接管；原判提升不成立，改记保留为局部不变量（不驱动公开黑盒）。 |
| `session_resume_variant_exists_in_enum` | `crates/lattice-cli/src/commands/session.rs:1297` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `session_prune_snapshots_variant_exists_in_enum` | `crates/lattice-cli/src/commands/session.rs:1306` | 删除 | 局部不变量 | n/a | 扫描源码字符串/import 的架构检查；删除（RFC #19 决定六） |
| `session_resume_handler_function_exists` | `crates/lattice-cli/src/commands/session.rs:1315` | 删除 | 局部不变量 | n/a | 扫描源码字符串/import（impl_source() + contains fn cmd_resume_api）的架构检查；删除（RFC #19 决定六）。 |
| `session_prune_handler_function_exists` | `crates/lattice-cli/src/commands/session.rs:1324` | 保留 | 局部不变量 | - | session 生命周期/prune/cleanup 局部不变量（无完整 turn 栈）；保留 |
| `session_resume_calls_typed_api` | `crates/lattice-cli/src/commands/session.rs:1333` | 删除 | 局部不变量 | n/a | 扫描源码字符串/import（impl_source() + contains resume_turn()）的架构检查；删除（RFC #19 决定六）。 |
| `session_prune_calls_typed_api` | `crates/lattice-cli/src/commands/session.rs:1342` | 保留 | 局部不变量 | - | session 生命周期/prune/cleanup 局部不变量（无完整 turn 栈）；保留 |
| `project_branch_new_without_required_flag_fails` | `crates/lattice-cli/src/commands/session.rs:1353` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `project_branch_new_with_required_flag_is_accepted` | `crates/lattice-cli/src/commands/session.rs:1364` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `project_branch_send_has_no_branch_override` | `crates/lattice-cli/src/commands/session.rs:1385` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `project_branch_send_source_has_no_branch_flag` | `crates/lattice-cli/src/commands/session.rs:1395` | 删除 | 局部不变量 | n/a | 扫描源码字符串/import 的架构检查；删除（RFC #19 决定六） |
| `project_branch_new_source_defines_field` | `crates/lattice-cli/src/commands/session.rs:1410` | 删除 | 局部不变量 | n/a | 扫描源码字符串/import 的架构检查（include_str! 扫描 SessionAction::New 字段）；删除（RFC #19 决定六）。 |
| `branch_listing_reads_only_addressed_session_refs` | `crates/lattice-cli/src/commands/session.rs:1511` | 提升 | Direct-Local 外部二进制 | `planned_direct_branch_listing_isolation` | 原内部测试断言集合由 #24 精确场景 `planned_direct_branch_listing_isolation` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `checkout_changes_only_addressed_session_head` | `crates/lattice-cli/src/commands/session.rs:1556` | 提升 | Direct-Local 外部二进制 | `planned_direct_checkout_isolation` | 原内部测试断言集合由 #24 精确场景 `planned_direct_checkout_isolation` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `branch_create_affects_only_addressed_session` | `crates/lattice-cli/src/commands/session.rs:1586` | 提升 | Direct-Local 外部二进制 | `planned_direct_branch_create_isolation` | 原内部测试断言集合由 #24 精确场景 `planned_direct_branch_create_isolation` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `reset_changes_only_addressed_session_branch_pointer` | `crates/lattice-cli/src/commands/session.rs:1613` | 提升 | Direct-Local 外部二进制 | `planned_direct_reset_isolation` | 原内部测试断言集合由 #24 精确场景 `planned_direct_reset_isolation` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `diff_resolves_refs_inside_addressed_session_only` | `crates/lattice-cli/src/commands/session.rs:1650` | 提升 | Direct-Local 外部二进制 | `planned_direct_diff_isolation` | 原内部测试断言集合由 #24 精确场景 `planned_direct_diff_isolation` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `boundary_no_legacy_session_store_patterns` | `crates/lattice-cli/src/commands/session.rs:1691` | 删除 | 局部不变量 | n/a | 扫描源码 import/crate 名/manifest 字符串；删除（RFC #19 决定六） |
| `project_branch_creation_json_output_includes_branch` | `crates/lattice-cli/src/commands/session.rs:1755` | 提升 | 快速二进制黑盒 | 冗余保留 `planned_session_cli` | 已删除原内部测试，由已落地 fast_session_cli（fast_session_new_json_reports_requested_project_branch：session new --project-branch feature/test --json 输出 project_branch=feature/test）黑盒接管 |
| `project_branch_status_output_includes_target_branch` | `crates/lattice-cli/src/commands/session.rs:1775` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `project_branch_list_output_includes_target_branch` | `crates/lattice-cli/src/commands/session.rs:1794` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `project_branch_persists_through_reopen` | `crates/lattice-cli/src/commands/session.rs:1816` | 提升 | 快速二进制黑盒 | 冗余保留 `planned_session_cli` | 已删除原内部测试，由已落地 fast_session_cli（fast_session_project_branch_persists_across_cli_invocations：独立第二次 CLI 进程读 session status --json 断言 project_target_branch=feature/persist；进程边界 reopen 严格强于库内 SessionApi::open_session 往返）黑盒接管 |
| `cli_surface_run_is_not_exposed` | `crates/lattice-cli/src/main.rs:153` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_new_agent` | `crates/lattice-cli/src/main.rs:158` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_send` | `crates/lattice-cli/src/main.rs:173` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_models_auth` | `crates/lattice-cli/src/main.rs:179` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_resolve` | `crates/lattice-cli/src/main.rs:185` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_status` | `crates/lattice-cli/src/main.rs:191` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_log` | `crates/lattice-cli/src/main.rs:197` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_checkout` | `crates/lattice-cli/src/main.rs:203` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_branch_with_from` | `crates/lattice-cli/src/main.rs:209` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_diff` | `crates/lattice-cli/src/main.rs:218` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_reset` | `crates/lattice-cli/src/main.rs:224` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_doctor` | `crates/lattice-cli/src/main.rs:230` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_rejects_unknown_command` | `crates/lattice-cli/src/main.rs:236` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_help_contains_supported_commands` | `crates/lattice-cli/src/main.rs:243` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_help_lists_exact_supported_command_count` | `crates/lattice-cli/src/main.rs:257` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_tui_requires_project_branch_for_new_session` | `crates/lattice-cli/src/main.rs:271` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_tui_parses_session_id_and_agent` | `crates/lattice-cli/src/main.rs:279` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_lancet_no_subcommand_parses` | `crates/lattice-cli/src/main.rs:296` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_lancet_status_parses` | `crates/lattice-cli/src/main.rs:306` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_lancet_doctor_parses` | `crates/lattice-cli/src/main.rs:312` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_doctor_unchanged` | `crates/lattice-cli/src/main.rs:318` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_audit_unchanged` | `crates/lattice-cli/src/main.rs:324` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_audit_replay_subcommand` | `crates/lattice-cli/src/main.rs:330` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_audit_query_subcommand` | `crates/lattice-cli/src/main.rs:345` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_tui_accepts_project_branch` | `crates/lattice-cli/src/main.rs:359` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_surface_session_new_requires_project_branch` | `crates/lattice-cli/src/main.rs:384` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_cutover_help_contains_supported_commands` | `crates/lattice-cli/tests/cli_cutover.rs:20` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_cutover_help_omits_removed_commands` | `crates/lattice-cli/tests/cli_cutover.rs:35` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_cutover_bare_lattice_prints_help_and_exits_zero` | `crates/lattice-cli/tests/cli_cutover.rs:76` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_cutover_rejects_unknown_command_with_clap_error` | `crates/lattice-cli/tests/cli_cutover.rs:94` | 保留 | 局部不变量 | - | CLI 参数解析/错误码映射局部契约；保留（与黑盒命令面互补，不重复产品路径） |
| `cli_cutover_resolve_json_excludes_secret_value` | `crates/lattice-cli/tests/cli_cutover.rs:111` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_cutover_models_auth_one_alias_per_line_without_icons_or_secrets` | `crates/lattice-cli/tests/cli_cutover.rs:148` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `cli_cutover_session_log_missing_session_fails_with_id_in_message` | `crates/lattice-cli/tests/cli_cutover.rs:197` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_cli（session new/list/status/log/branch --json） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `focused_tests_import_no_removed_crate_names` | `crates/lattice-cli/tests/cli_cutover.rs:224` | 删除 | 局部不变量 | n/a | 扫描源码 import/crate 名/manifest 字符串；删除（RFC #19 决定六） |
| `cli_cutover_run_command_is_removed` | `crates/lattice-cli/tests/cli_cutover.rs:328` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `fast_cli_help` 接管 | 已删除原内部测试，由已落地 fast_cli_help（CLI help 表面） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_models_auth_lists_aliases_without_secrets` | `crates/lattice-cli/tests/release_smoke.rs:100` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_resolve_sonnet_json_shows_env_var_not_secret` | `crates/lattice-cli/tests/release_smoke.rs:131` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_resolve_missing_credential_names_env_var` | `crates/lattice-cli/tests/release_smoke.rs:159` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_session_status_list_shows_seeded_session` | `crates/lattice-cli/tests/release_smoke.rs:185` | 提升 | 快速二进制黑盒 | 冗余保留 `planned_session_cli` | 已删除原内部测试，由已落地 fast_session_cli（fast_session_new_creates_session_and_appears_in_list：session new 后 session list --json 含 session_id）黑盒接管；seed_session 的 committed turn 是测试夹具而非断言对象 |
| `smoke_session_status_specific_session` | `crates/lattice-cli/tests/release_smoke.rs:210` | 提升 | 快速二进制黑盒 | 冗余保留 `planned_session_cli` | 已删除原内部测试，由已落地 fast_session_cli（fast_session_status_new_session_shows_zero_commits_and_agent + fast_session_status_json_stable_contract：session status <id> 文本/JSON 双模断言 session_id）黑盒接管；seed_session 的 committed turn 是测试夹具而非断言对象 |
| `smoke_session_log_shows_seeded_turn` | `crates/lattice-cli/tests/release_smoke.rs:242` | 提升 | Direct-Local 外部二进制 | `planned_direct_session_log_turn` | 原内部测试断言集合由 #24 精确场景 `planned_direct_session_log_turn` 接管（assertion-level，非 family fallback）；待 #24 落地后删除原测试。 |
| `smoke_doctor_runs_without_leaking_secrets` | `crates/lattice-cli/tests/release_smoke.rs:269` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_doctor_json_does_not_leak_secrets` | `crates/lattice-cli/tests/release_smoke.rs:299` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_config_cli` 接管 | 已删除原内部测试，由已落地 fast_config_cli（models/resolve/doctor + secret redaction） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `smoke_session_send_missing_credentials_fails_without_secrets` | `crates/lattice-cli/tests/release_smoke.rs:329` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原弱 smoke，由已落地 fast_session_send_missing_credential_is_redacted_input_error 接管：凭据解析早于 prepare_upper，无需 KVM；精确断言 exit 2、缺失 env-var 名与 stdout/stderr 无 secret 泄漏（#23 review-fix round-6）。 |
| `smoke_session_send_no_profile_fails_with_clear_message` | `crates/lattice-cli/tests/release_smoke.rs:354` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_send_no_profile（session send 在 load_agent_checked 失败：创建 session 后删除 .lattice/agents/ 触发 agent profile 缺失错误，exit 2 + stderr 含 profile/agent.toml，不泄 secret）黑盒接管（#23 review-fix round-3）；Upper 前失败，逐条映射。 |
| `smoke_session_status_nonexistent_returns_error` | `crates/lattice-cli/tests/release_smoke.rs:384` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_cli（session new/list/status/log/branch --json） 黑盒接管（#23 批次）；逐条映射、消除三元泛化替代。 |
| `release_smoke_release_gate_sh_is_retired` | `crates/lattice-cli/tests/release_smoke.rs:407` | 保留 | 局部不变量 | - | #23 复审偏差：运行 release-gate.sh 并断言 nonzero exit + retired stderr，是脚本退役 smoke，非 CLI help 表面行为；无已落地或 planned 场景能诚实接管，改记保留为局部不变量。 |
| `release_smoke_release_gate_v1_4_includes_lancet_boundary_check` | `crates/lattice-cli/tests/release_smoke.rs:432` | 保留 | 局部不变量 | - | #23 复审偏差：源码扫描 release-gate-v1.4.sh 脚本文件内容（asserts contains lancet-boundary 等），是 repo tooling 局部不变量，非 CLI help 表面行为；无已落地或 planned 场景能诚实接管，改记保留为局部不变量。 |
| `smoke_session_resume_without_session_id_fails` | `crates/lattice-cli/tests/release_smoke.rs:459` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_resume_without_id（session resume 缺参数 clap 拒绝，exit 2 + stderr）黑盒接管（#23 review-fix round-3）；Upper 前失败，逐条映射。 |
| `smoke_session_prune_snapshots_without_session_id_fails` | `crates/lattice-cli/tests/release_smoke.rs:481` | 保留 | 局部不变量 | - | session 生命周期/prune/cleanup 局部不变量（无完整 turn 栈）；保留 |
| `smoke_session_resume_nonexistent_session_returns_error` | `crates/lattice-cli/tests/release_smoke.rs:503` | 提升 | 快速二进制黑盒 | 已删除原测试，由 `planned_session_cli` 接管 | 已删除原内部测试，由已落地 fast_session_resume_nonexistent_session（session resume 不存在 session，调用错误 exit 2 + stderr 含 not found 与 session ID）黑盒接管（#23 review-fix round-5）；Upper 前失败，逐条映射。 |

### `lattice-llm`（保留 133 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `missing_credential_returns_named_error` | `crates/lattice-llm/src/client.rs:519` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `present_credential_reads_key_value` | `crates/lattice-llm/src/client.rs:539` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `missing_credential_error_never_leaks_key_value` | `crates/lattice-llm/src/client.rs:560` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `empty_credential_returns_error` | `crates/lattice-llm/src/client.rs:586` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `dispatch_does_not_use_registry_or_plugins` | `crates/lattice-llm/src/client.rs:602` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `dispatch_requires_base_url_validation` | `crates/lattice-llm/src/client.rs:613` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `openai_auth_headers_use_bearer` | `crates/lattice-llm/src/client.rs:627` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `anthropic_auth_headers_use_x_api_key` | `crates/lattice-llm/src/client.rs:643` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `gemini_auth_headers_use_x_goog_api_key` | `crates/lattice-llm/src/client.rs:661` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `openai_endpoint_url` | `crates/lattice-llm/src/client.rs:677` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `anthropic_endpoint_url` | `crates/lattice-llm/src/client.rs:691` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `gemini_endpoint_url` | `crates/lattice-llm/src/client.rs:705` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `bounded_error_chunk_reports_truncation` | `crates/lattice-llm/src/client.rs:720` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `bounded_error_chunk_accepts_limit_exactly` | `crates/lattice-llm/src/client.rs:729` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `find_event_boundary_lf_lf` | `crates/lattice-llm/src/client.rs:737` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `find_event_boundary_crlf_crlf` | `crates/lattice-llm/src/client.rs:745` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `find_event_boundary_cr_cr` | `crates/lattice-llm/src/client.rs:753` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `drain_preserves_split_utf8_across_chunks` | `crates/lattice-llm/src/client.rs:761` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `drain_handles_crlf_event_separator` | `crates/lattice-llm/src/client.rs:808` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `drain_allows_large_cumulative_stream_when_events_are_framed` | `crates/lattice-llm/src/client.rs:827` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_model_and_max_tokens` | `crates/lattice-llm/src/providers/anthropic.rs:302` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_extracts_system_message` | `crates/lattice-llm/src/providers/anthropic.rs:311` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_merges_multiple_system_messages` | `crates/lattice-llm/src/providers/anthropic.rs:328` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_user_message_content_blocks` | `crates/lattice-llm/src/providers/anthropic.rs:347` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_assistant_with_tool_use` | `crates/lattice-llm/src/providers/anthropic.rs:367` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_tool_result` | `crates/lattice-llm/src/providers/anthropic.rs:399` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_tools_use_input_schema` | `crates/lattice-llm/src/providers/anthropic.rs:426` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_always_streams` | `crates/lattice-llm/src/providers/anthropic.rs:449` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_uses_explicit_max_tokens` | `crates/lattice-llm/src/providers/anthropic.rs:467` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `auth_header_is_api_key_value` | `crates/lattice-llm/src/providers/anthropic.rs:482` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `anthropic_version_constant` | `crates/lattice-llm/src/providers/anthropic.rs:488` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_content_block_delta_text` | `crates/lattice-llm/src/providers/anthropic.rs:493` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_content_block_start_tool_use` | `crates/lattice-llm/src/providers/anthropic.rs:502` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_content_block_delta_json` | `crates/lattice-llm/src/providers/anthropic.rs:511` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_content_block_stop_tool_use` | `crates/lattice-llm/src/providers/anthropic.rs:523` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_message_delta_done` | `crates/lattice-llm/src/providers/anthropic.rs:535` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_message_start_tracks_input_tokens` | `crates/lattice-llm/src/providers/anthropic.rs:550` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_message_stop_ignored` | `crates/lattice-llm/src/providers/anthropic.rs:558` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_ping_ignored` | `crates/lattice-llm/src/providers/anthropic.rs:565` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_error_event` | `crates/lattice-llm/src/providers/anthropic.rs:572` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_thinking_delta` | `crates/lattice-llm/src/providers/anthropic.rs:583` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `anthropic_provider_has_no_lattice_core_imports` | `crates/lattice-llm/src/providers/anthropic.rs:598` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_system_instruction` | `crates/lattice-llm/src/providers/gemini.rs:340` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_maps_user_role` | `crates/lattice-llm/src/providers/gemini.rs:358` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_maps_assistant_to_model_role` | `crates/lattice-llm/src/providers/gemini.rs:374` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_maps_function_call` | `crates/lattice-llm/src/providers/gemini.rs:390` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_maps_function_response` | `crates/lattice-llm/src/providers/gemini.rs:424` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_tools_as_function_declarations` | `crates/lattice-llm/src/providers/gemini.rs:455` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_stream_url_includes_url_encoded_model` | `crates/lattice-llm/src/providers/gemini.rs:476` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_stream_url_encodes_special_chars_in_model` | `crates/lattice-llm/src/providers/gemini.rs:496` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `auth_header_name_is_x_goog_api_key` | `crates/lattice-llm/src/providers/gemini.rs:506` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_token_chunk` | `crates/lattice-llm/src/providers/gemini.rs:512` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_reasoning_chunk` | `crates/lattice-llm/src/providers/gemini.rs:523` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_function_call` | `crates/lattice-llm/src/providers/gemini.rs:534` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_done_with_finish_reason_and_usage` | `crates/lattice-llm/src/providers/gemini.rs:545` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_error_chunk` | `crates/lattice-llm/src/providers/gemini.rs:554` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_empty_data` | `crates/lattice-llm/src/providers/gemini.rs:565` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `gemini_provider_has_no_lattice_core_imports` | `crates/lattice-llm/src/providers/gemini.rs:572` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_valid_json_returns_some` | `crates/lattice-llm/src/providers/mod.rs:93` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_empty_string_returns_none` | `crates/lattice-llm/src/providers/mod.rs:99` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_whitespace_only_returns_none` | `crates/lattice-llm/src/providers/mod.rs:105` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_done_sentinel_returns_none` | `crates/lattice-llm/src/providers/mod.rs:111` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_done_with_whitespace_returns_none` | `crates/lattice-llm/src/providers/mod.rs:117` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_invalid_json_returns_error` | `crates/lattice-llm/src/providers/mod.rs:123` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_model` | `crates/lattice-llm/src/providers/openai.rs:264` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_messages_with_roles` | `crates/lattice-llm/src/providers/openai.rs:272` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_always_streams` | `crates/lattice-llm/src/providers/openai.rs:292` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `wire_tool_name_maps_dotted_cep_names` | `crates/lattice-llm/src/providers/openai.rs:308` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_maps_dotted_tool_names_for_openai_wire` | `crates/lattice-llm/src/providers/openai.rs:318` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_tools` | `crates/lattice-llm/src/providers/openai.rs:336` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_temperature_and_max_tokens` | `crates/lattice-llm/src/providers/openai.rs:362` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_omits_optional_fields_when_none` | `crates/lattice-llm/src/providers/openai.rs:378` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_assistant_tool_calls` | `crates/lattice-llm/src/providers/openai.rs:387` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `build_request_includes_tool_result` | `crates/lattice-llm/src/providers/openai.rs:417` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `auth_header_uses_bearer_prefix` | `crates/lattice-llm/src/providers/openai.rs:441` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `auth_header_contains_key_value` | `crates/lattice-llm/src/providers/openai.rs:451` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_token_chunk` | `crates/lattice-llm/src/providers/openai.rs:460` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_reasoning_chunk` | `crates/lattice-llm/src/providers/openai.rs:469` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_tool_call_start` | `crates/lattice-llm/src/providers/openai.rs:480` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_tool_call_delta` | `crates/lattice-llm/src/providers/openai.rs:489` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_done_with_finish_reason_and_usage` | `crates/lattice-llm/src/providers/openai.rs:500` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_done_sentinel_ignored` | `crates/lattice-llm/src/providers/openai.rs:509` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_error_chunk` | `crates/lattice-llm/src/providers/openai.rs:516` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_tool_call_end_on_finish` | `crates/lattice-llm/src/providers/openai.rs:527` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_sse_empty_data_returns_empty` | `crates/lattice-llm/src/providers/openai.rs:544` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `openai_provider_has_no_lattice_core_imports` | `crates/lattice-llm/src/providers/openai.rs:551` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `credential_status_present_from_env` | `crates/lattice-llm/src/router.rs:224` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `credential_status_missing_when_unset` | `crates/lattice-llm/src/router.rs:235` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `credential_status_missing_when_empty` | `crates/lattice-llm/src/router.rs:243` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_known_aliases` | `crates/lattice-llm/src/router.rs:258` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `provider_kind_as_str_is_stable_snake_case` | `crates/lattice-llm/src/router.rs:322` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_unknown_alias_returns_error` | `crates/lattice-llm/src/router.rs:330` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_unknown_alias_error_is_not_fallback` | `crates/lattice-llm/src/router.rs:338` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolved_model_debug_does_not_leak_secrets` | `crates/lattice-llm/src/router.rs:353` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolved_model_serialization_does_not_leak_secrets` | `crates/lattice-llm/src/router.rs:372` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `model_router_default_has_sonnet_default` | `crates/lattice-llm/src/router.rs:384` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `aliases_returns_all_known_aliases` | `crates/lattice-llm/src/router.rs:390` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `aliases_carry_provider_metadata_and_no_secrets` | `crates/lattice-llm/src/router.rs:422` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_compat_prefix_model` | `crates/lattice-llm/src/router.rs:459` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_openai_compatible_prefix_model` | `crates/lattice-llm/src/router.rs:470` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_compatible_prefix_model` | `crates/lattice-llm/src/router.rs:480` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_bare_openai_compatible_uses_env_model` | `crates/lattice-llm/src/router.rs:489` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_bare_openai_compatible_errors_without_env_model` | `crates/lattice-llm/src/router.rs:504` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `resolve_empty_compat_model_errors` | `crates/lattice-llm/src/router.rs:526` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_simple_event` | `crates/lattice-llm/src/sse.rs:105` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_multi_line_data` | `crates/lattice-llm/src/sse.rs:114` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_event_with_type` | `crates/lattice-llm/src/sse.rs:122` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `skip_comments` | `crates/lattice-llm/src/sse.rs:131` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `done_marker_is_just_data` | `crates/lattice-llm/src/sse.rs:139` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `reject_data_size_exceeded` | `crates/lattice-llm/src/sse.rs:148` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `reject_event_count_exceeded` | `crates/lattice-llm/src/sse.rs:155` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `parse_event_with_id` | `crates/lattice-llm/src/sse.rs:168` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `empty_input_yields_empty` | `crates/lattice-llm/src/sse.rs:177` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_returns_stub_response_content` | `crates/lattice-llm/src/stub.rs:290` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_default_stub_produces_deterministic_response` | `crates/lattice-llm/src/stub.rs:302` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `stream_yields_started_token_done_sequence` | `crates/lattice-llm/src/stub.rs:313` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `stream_token_contains_stub_response` | `crates/lattice-llm/src/stub.rs:341` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_collects_stream` | `crates/lattice-llm/src/stub.rs:366` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_still_works_for_empty_messages` | `crates/lattice-llm/src/stub.rs:377` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `stream_resolves_model_before_yielding` | `crates/lattice-llm/src/stub.rs:385` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `stub_llm_no_lattice_core_references` | `crates/lattice-llm/src/stub.rs:399` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `role_serde_roundtrip` | `crates/lattice-llm/src/types.rs:244` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `role_serialization_variants` | `crates/lattice-llm/src/types.rs:254` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `function_call_roundtrip` | `crates/lattice-llm/src/types.rs:265` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `tool_call_roundtrip` | `crates/lattice-llm/src/types.rs:273` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `message_simple_roundtrip` | `crates/lattice-llm/src/types.rs:284` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `message_with_tool_calls_roundtrip` | `crates/lattice-llm/src/types.rs:292` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `message_tool_result_roundtrip` | `crates/lattice-llm/src/types.rs:310` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `tool_definition_roundtrip` | `crates/lattice-llm/src/types.rs:325` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_request_roundtrip` | `crates/lattice-llm/src/types.rs:343` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `chat_response_roundtrip` | `crates/lattice-llm/src/types.rs:359` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `llm_event_roundtrip` | `crates/lattice-llm/src/types.rs:374` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |
| `model_target_default` | `crates/lattice-llm/src/types.rs:393` | 保留 | 局部不变量 | — | provider 请求构造/SSE parser/路由/HTTP pinning 局部不变量；保留 |

### `lattice-profile`（保留 96 / 提升 0 / 删除 1）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `template_parses_as_valid_v1_profile` | `crates/lattice-profile/src/defaults.rs:207` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_escapes_name_and_model_as_toml_strings` | `crates/lattice-profile/src/defaults.rs:227` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_contains_auto_generated_comment` | `crates/lattice-profile/src/defaults.rs:236` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_includes_mandatory_workspace_section` | `crates/lattice-profile/src/defaults.rs:249` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_has_only_allowed_top_level_sections` | `crates/lattice-profile/src/defaults.rs:275` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_has_no_bundles_key` | `crates/lattice-profile/src/defaults.rs:293` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_has_no_forbidden_legacy_concepts` | `crates/lattice-profile/src/defaults.rs:302` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `template_does_not_create_files` | `crates/lattice-profile/src/defaults.rs:332` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `generated_profiles_have_mandatory_workspace_and_valid_tools` | `crates/lattice-profile/src/defaults.rs:340` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reader_template_parses_as_valid_v1_profile` | `crates/lattice-profile/src/defaults.rs:366` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reader_template_enabled_tools_are_resolvable` | `crates/lattice-profile/src/defaults.rs:380` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `editor_template_parses_as_valid_v1_profile` | `crates/lattice-profile/src/defaults.rs:395` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `editor_template_has_no_web_fetch_tool` | `crates/lattice-profile/src/defaults.rs:410` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `dev_template_parses_as_valid_v1_profile` | `crates/lattice-profile/src/defaults.rs:418` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `dev_template_enabled_tools_are_resolvable` | `crates/lattice-profile/src/defaults.rs:432` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reader_editor_dev_templates_have_no_forbidden_legacy_concepts` | `crates/lattice-profile/src/defaults.rs:445` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `all_templates_have_no_bundles_key` | `crates/lattice-profile/src/defaults.rs:459` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `all_templates_have_non_empty_enabled_tools` | `crates/lattice-profile/src/defaults.rs:471` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `default_templates_use_only_dotted_catalog_names` | `crates/lattice-profile/src/defaults.rs:488` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reader_editor_dev_templates_do_not_create_files` | `crates/lattice-profile/src/defaults.rs:504` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `explicit_path_wins_over_named_and_default` | `crates/lattice-profile/src/discovery.rs:134` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `named_lookup_prefers_project_local` | `crates/lattice-profile/src/discovery.rs:150` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `named_lookup_falls_back_to_global` | `crates/lattice-profile/src/discovery.rs:160` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `default_checks_project_first_then_global` | `crates/lattice-profile/src/discovery.rs:169` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `missing_default_returns_typed_error_with_template` | `crates/lattice-profile/src/discovery.rs:186` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `missing_default_creates_no_files` | `crates/lattice-profile/src/discovery.rs:198` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `named_missing_returns_profile_not_found_error` | `crates/lattice-profile/src/discovery.rs:211` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `nonexistent_agent_dir_is_handled_as_missing` | `crates/lattice-profile/src/discovery.rs:223` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `load_inline_system_prompt` | `crates/lattice-profile/src/load.rs:248` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `load_system_path_prompt` | `crates/lattice-profile/src/load.rs:269` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_both_system_and_system_path` | `crates/lattice-profile/src/load.rs:297` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_neither_system_nor_system_path` | `crates/lattice-profile/src/load.rs:318` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_missing_prompt_file` | `crates/lattice-profile/src/load.rs:337` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_absolute_system_path` | `crates/lattice-profile/src/load.rs:361` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_dot_dot_traversal` | `crates/lattice-profile/src/load.rs:385` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_path_escape_via_symlink` | `crates/lattice-profile/src/load.rs:409` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_prompt_path_that_is_directory` | `crates/lattice-profile/src/load.rs:440` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `loading_is_read_only_no_side_files` | `crates/lattice-profile/src/load.rs:467` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_schema_version_0` | `crates/lattice-profile/src/load.rs:499` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_schema_version_2` | `crates/lattice-profile/src/load.rs:523` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_forbidden_sections_in_loader` | `crates/lattice-profile/src/load.rs:547` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `llm_retry_max_retries_zero_is_valid` | `crates/lattice-profile/src/load.rs:585` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_invalid_llm_retry_bounds` | `crates/lattice-profile/src/load.rs:610` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `schema_version_must_be_1` | `crates/lattice-profile/src/load.rs:652` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `all_forbidden_sections_rejected` | `crates/lattice-profile/src/load.rs:680` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `valid_minimal_profile` | `crates/lattice-profile/src/schema.rs:436` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `valid_full_profile` | `crates/lattice-profile/src/schema.rs:462` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_unknown_top_level_section` | `crates/lattice-profile/src/schema.rs:500` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_security_section` | `crates/lattice-profile/src/schema.rs:519` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_context_section` | `crates/lattice-profile/src/schema.rs:538` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_behavior_section` | `crates/lattice-profile/src/schema.rs:557` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_capabilities_section` | `crates/lattice-profile/src/schema.rs:576` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_handoff_section` | `crates/lattice-profile/src/schema.rs:595` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_all_forbidden_top_level_sections` | `crates/lattice-profile/src/schema.rs:613` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_tools_bundles` | `crates/lattice-profile/src/schema.rs:653` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_agent_preset_field` | `crates/lattice-profile/src/schema.rs:677` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_agent_type_code_agent` | `crates/lattice-profile/src/schema.rs:694` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_agent_skippable` | `crates/lattice-profile/src/schema.rs:710` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `reject_agent_tags` | `crates/lattice-profile/src/schema.rs:726` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_defaults_to_enforce_when_section_absent` | `crates/lattice-profile/src/schema.rs:763` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_parses_enforce_mode` | `crates/lattice-profile/src/schema.rs:770` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_parses_warn_mode` | `crates/lattice-profile/src/schema.rs:786` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_parses_off_mode` | `crates/lattice-profile/src/schema.rs:802` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_rejects_invalid_mode` | `crates/lattice-profile/src/schema.rs:818` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_rejects_scalar_extension_typos` | `crates/lattice-profile/src/schema.rs:833` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_parses_typed_prof02_sub_tables` | `crates/lattice-profile/src/schema.rs:849` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_preserves_untyped_future_sub_tables_in_extensions` | `crates/lattice-profile/src/schema.rs:895` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_older_profile_mode_only_still_parses` | `crates/lattice-profile/src/schema.rs:918` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_observation_rejects_network_only_field` | `crates/lattice-profile/src/schema.rs:937` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_serializes_extensions_flattened` | `crates/lattice-profile/src/schema.rs:956` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `tools_section_defaults_to_empty_enabled` | `crates/lattice-profile/src/schema.rs:982` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `llm_retry_defaults_when_section_absent` | `crates/lattice-profile/src/schema.rs:1000` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `llm_retry_parses_explicit_values` | `crates/lattice-profile/src/schema.rs:1008` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_section_missing_fails` | `crates/lattice-profile/src/schema.rs:1030` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_egress_blocked_parses` | `crates/lattice-profile/src/schema.rs:1041` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_egress_allowlist_with_targets` | `crates/lattice-profile/src/schema.rs:1059` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_credentials_name_only` | `crates/lattice-profile/src/schema.rs:1080` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_unknown_field_rejected` | `crates/lattice-profile/src/schema.rs:1113` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `lancet_workspace_roundtrip` | `crates/lattice-profile/src/schema.rs:1132` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `lancet_workspace_unknown_field_rejected` | `crates/lattice-profile/src/schema.rs:1154` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `lancet_verify_section_is_not_a_typed_field` | `crates/lattice-profile/src/schema.rs:1170` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_image_optional_without_provisioner_default` | `crates/lattice-profile/src/schema.rs:1188` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_roundtrip` | `crates/lattice-profile/src/schema.rs:1204` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `source_free_of_forbidden_symbols` | `crates/lattice-profile/src/schema.rs:1245` | 删除 | 局部不变量 | n/a — 无替代场景（源码字符串扫描；边界由 xtask/依赖图负责） | 扫描源码字符串/import 的架构检查；删除（RFC #19 决定六） |
| `cep_policy_section_parses_path` | `crates/lattice-profile/src/schema.rs:1276` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_section_without_policy_defaults_to_none` | `crates/lattice-profile/src/schema.rs:1293` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `cep_policy_section_rejects_unknown_fields` | `crates/lattice-profile/src/schema.rs:1299` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_lifecycle_limits_defaults_when_omitted` | `crates/lattice-profile/src/schema.rs:1320` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_lifecycle_limits_explicit_values_roundtrip` | `crates/lattice-profile/src/schema.rs:1339` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_lifecycle_limits_boundary_values_accepted` | `crates/lattice-profile/src/schema.rs:1364` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_lifecycle_limits_zero_rejected` | `crates/lattice-profile/src/schema.rs:1395` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_lifecycle_limits_out_of_range_rejected` | `crates/lattice-profile/src/schema.rs:1426` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_typo_cleanup_round_rejected` | `crates/lattice-profile/src/schema.rs:1459` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_typo_max_pending_snapshot_rejected` | `crates/lattice-profile/src/schema.rs:1477` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_serialization_contains_resolved_lifecycle_values` | `crates/lattice-profile/src/schema.rs:1496` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_serialization_contains_no_secret_values` | `crates/lattice-profile/src/schema.rs:1521` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |
| `workspace_mandatory_section_present_in_minimal_prefix` | `crates/lattice-profile/src/schema.rs:1565` | 保留 | 局部不变量 | — | agent.toml schema/defaults/校验局部不变量；保留 |

### `lattice-session`（保留 69 / 提升 0 / 删除 2）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `test_commit_turn_tree_structure` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:10` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_commit_turn_linear` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:49` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_commit_turn_with_parent` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:67` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_get_turn_commit` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:86` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_get_turn_commit_not_found` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:102` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_set_get_snapshot_lost` | `crates/lattice-session/src/git_graph/tests/commit_turn.rs:117` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_head_transcript_linear` | `crates/lattice-session/src/git_graph/tests/head_transcript.rs:9` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_head_transcript_empty_session` | `crates/lattice-session/src/git_graph/tests/head_transcript.rs:28` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_no_remote_push_refspec` | `crates/lattice-session/src/git_graph/tests/no_remote.rs:14` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_branch_create` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:13` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_branch_from_commit` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:34` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_branch_duplicate_rejected` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:48` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_checkout_branch` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:62` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_checkout_detached` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:81` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_reset` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:95` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_reset_detached_conflict` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:119` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_diff_linear` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:134` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_diff_merge_base` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:152` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_diff_no_common_ancestor` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:200` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_candidate_ref_created` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:228` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_head_transcript_branched` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:270` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_head_transcript_reset` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:300` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_branch_reset_checkout_diff` | `crates/lattice-session/src/git_graph/tests/pointer_ops.rs:315` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_create_session` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:10` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_open_session` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:32` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_open_session_not_found` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:46` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_set_active_agent` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:56` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_list_sessions` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:74` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_status_empty_session` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:93` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `test_repo_auto_init_bare_on_fresh_dir` | `crates/lattice-session/src/git_graph/tests/session_meta.rs:108` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `graph_types_exist_and_serialize` | `crates/lattice-session/src/graph.rs:712` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `turn_commit_input_and_turn_commit_serialize` | `crates/lattice-session/src/graph.rs:768` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `session_status_and_summary_serialize` | `crates/lattice-session/src/graph.rs:818` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `graph_diff_and_reset_result_serialize` | `crates/lattice-session/src/graph.rs:847` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `turn_log_entry_serializes` | `crates/lattice-session/src/graph.rs:868` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `session_graph_store_trait_does_not_include_legacy_operations` | `crates/lattice-session/src/graph.rs:889` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_normalizes_whitespace` | `crates/lattice-session/src/graph.rs:899` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_truncates_long_input` | `crates/lattice-session/src/graph.rs:905` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_fallback_for_empty_messages` | `crates/lattice-session/src/graph.rs:914` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_fallback_for_empty_user_text` | `crates/lattice-session/src/graph.rs:919` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_fallback_for_no_user_role` | `crates/lattice-session/src/graph.rs:925` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `derive_commit_message_picks_first_user_message` | `crates/lattice-session/src/graph.rs:931` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_turn_commit_id_includes_logical_fields` | `crates/lattice-session/src/graph.rs:942` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_turn_commit_id_deterministic` | `crates/lattice-session/src/graph.rs:961` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_turn_commit_id_changes_with_different_inputs` | `crates/lattice-session/src/graph.rs:984` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_turn_commit_id_excludes_storage_metadata` | `crates/lattice-session/src/graph.rs:1044` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_message_id_is_deterministic` | `crates/lattice-session/src/graph.rs:1066` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_message_id_differs_by_content` | `crates/lattice-session/src/graph.rs:1075` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_message_id_differs_by_ordinal` | `crates/lattice-session/src/graph.rs:1085` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_message_id_differs_by_session` | `crates/lattice-session/src/graph.rs:1096` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `canonical_message_id_differs_by_turn_seed` | `crates/lattice-session/src/graph.rs:1107` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `head_transcript_contract_serializes` | `crates/lattice-session/src/graph.rs:1118` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `head_transcript_parent_is_none_for_empty_session` | `crates/lattice-session/src/graph.rs:1139` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `turn_usage_serializes_for_commit_metadata` | `crates/lattice-session/src/graph.rs:1152` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `turn_commit_input_accepts_usage_and_terminal_reason` | `crates/lattice-session/src/graph.rs:1171` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `topology_equivalence_linear` | `crates/lattice-session/src/migrate/tests.rs:353` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `topology_equivalence_branched` | `crates/lattice-session/src/migrate/tests.rs:375` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `topology_equivalence_reset` | `crates/lattice-session/src/migrate/tests.rs:402` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `transcript_equivalence_linear` | `crates/lattice-session/src/migrate/tests.rs:426` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `restart_idempotent` | `crates/lattice-session/src/migrate/tests.rs:484` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `restart_recovers_from_partial_write` | `crates/lattice-session/src/migrate/tests.rs:555` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `native_manifest_oid_stable` | `crates/lattice-session/src/migrate/tests.rs:621` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `no_sqlite_write_path_in_production` | `crates/lattice-session/src/migrate/tests.rs:700` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `find_legacy_session_db_probes_session_v12_db_first` | `crates/lattice-session/src/migrate/tests.rs:747` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `migration_works_with_session_v12_db_filename` | `crates/lattice-session/src/migrate/tests.rs:780` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `dry_run_creates_no_target_skeleton` | `crates/lattice-session/src/migrate/tests.rs:828` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `verify_only_errors_when_target_absent` | `crates/lattice-session/src/migrate/tests.rs:869` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `session_commit_object_is_deterministic_and_ref_neutral` | `crates/lattice-session/tests/promotion_commit.rs:107` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `session_commit_object_links_optional_parent` | `crates/lattice-session/tests/promotion_commit.rs:146` | 保留 | 局部不变量 | — | session graph/Git 事务/并发/恢复/原子性局部不变量；保留 |
| `promotion_crate_has_no_workspace_or_api_dependency` | `crates/lattice-session/tests/promotion_commit.rs:178` | 删除 | 局部不变量 | n/a — 无替代场景（架构边界由 xtask/harness-boundary 负责） | 扫描源码 import/crate 名/manifest 字符串；删除（RFC #19 决定六） |
| `promotion_module_is_object_only` | `crates/lattice-session/tests/promotion_commit.rs:191` | 删除 | 局部不变量 | n/a — 无替代场景（源码字符串扫描；边界由 xtask/依赖图负责） | 扫描源码字符串/import 的架构检查；删除（RFC #19 决定六） |

### `lattice-system-harness`（保留 56 / 提升 1 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `write_fake_llm_log_renders_one_block_per_capture` | `crates/lattice-system-harness/src/artifacts.rs:351` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_fake_llm_log_empty_is_noop` | `crates/lattice-system-harness/src/artifacts.rs:377` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_fake_llm_log_redacts_secrets` | `crates/lattice-system-harness/src/artifacts.rs:384` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_workspace_listing_lists_files_and_dirs` | `crates/lattice-system-harness/src/artifacts.rs:399` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `workspace_snapshot_capture_is_sorted` | `crates/lattice-system-harness/src/artifacts.rs:420` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_workspace_diff_reports_added_removed_and_changed` | `crates/lattice-system-harness/src/artifacts.rs:438` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_workspace_diff_redacts_secrets` | `crates/lattice-system-harness/src/artifacts.rs:473` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_world_context_writes_scenario_command_paths_and_env` | `crates/lattice-system-harness/src/artifacts.rs:490` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `write_world_context_redacts_secret_env_value` | `crates/lattice-system-harness/src/artifacts.rs:511` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `scan_artifacts_for_secrets_no_leak_returns_empty` | `crates/lattice-system-harness/src/artifacts.rs:523` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `scan_artifacts_for_secrets_patches_leaked_file_in_place` | `crates/lattice-system-harness/src/artifacts.rs:532` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `scan_artifacts_for_secrets_skips_non_utf8_files` | `crates/lattice-system-harness/src/artifacts.rs:552` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `scan_artifacts_for_secrets_empty_secret_list_is_noop` | `crates/lattice-system-harness/src/artifacts.rs:568` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `run_child_in_own_process_group_returns_exit_status` | `crates/lattice-system-harness/src/driver.rs:279` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `timed_out_child_is_killed_and_timeout_surfaces` | `crates/lattice-system-harness/src/driver.rs:296` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `timed_out_process_group_kills_grandchild` | `crates/lattice-system-harness/src/driver.rs:328` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `processes_in_pgid_excludes_zombies` | `crates/lattice-system-harness/src/driver.rs:367` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `render_plain_text_script_matches_openai_shape` | `crates/lattice-system-harness/src/fake_llm.rs:460` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `render_tool_call_script_emits_start_args_and_tool_calls_finish` | `crates/lattice-system-harness/src/fake_llm.rs:476` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `render_disconnect_script_omits_terminal_chunk_and_done` | `crates/lattice-system-harness/src/fake_llm.rs:493` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `render_illegal_response_script_emits_non_json_data` | `crates/lattice-system-harness/src/fake_llm.rs:506` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_body_surfaces_model_roles_and_tool_names` | `crates/lattice-system-harness/src/fake_llm.rs:517` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_body_records_raw_text_when_not_json` | `crates/lattice-system-harness/src/fake_llm.rs:542` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `server_captures_request_and_returns_plain_text_sse` | `crates/lattice-system-harness/src/fake_llm.rs:557` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `msb_requested_is_false_when_unset` | `crates/lattice-system-harness/src/layer_guard.rs:117` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `msb_requested_is_true_when_one` | `crates/lattice-system-harness/src/layer_guard.rs:130` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `strip_ansi_removes_color_and_cursor_codes` | `crates/lattice-system-harness/src/pty_tui.rs:301` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `strip_ansi_handles_osc_title_sequence` | `crates/lattice-system-harness/src/pty_tui.rs:307` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `strip_ansi_handles_st_terminated_osc` | `crates/lattice-system-harness/src/pty_tui.rs:313` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `strip_ansi_passes_plain_text_through` | `crates/lattice-system-harness/src/pty_tui.rs:319` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `pty_round_trip_with_cat` | `crates/lattice-system-harness/src/pty_tui.rs:329` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `budget_with_no_limit_is_noop_and_never_panics` | `crates/lattice-system-harness/src/scenario.rs:134` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `budget_with_elapsed_limit_panics` | `crates/lattice-system-harness/src/scenario.rs:142` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `budget_within_limit_does_not_panic` | `crates/lattice-system-harness/src/scenario.rs:153` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `spec_total_timeout_round_trips` | `crates/lattice-system-harness/src/scenario.rs:159` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `spec_without_total_timeout_yields_no_budget` | `crates/lattice-system-harness/src/scenario.rs:172` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_turn_started` | `crates/lattice-system-harness/src/session_send.rs:562` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_token_delta` | `crates/lattice-system-harness/src/session_send.rs:576` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_turn_committed_completed` | `crates/lattice-system-harness/src/session_send.rs:587` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_unknown_variant_is_tolerated` | `crates/lattice-system-harness/src/session_send.rs:602` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `blank_and_non_json_lines_are_skipped` | `crates/lattice-system-harness/src/session_send.rs:614` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `exit_code_classification` | `crates/lattice-system-harness/src/session_send.rs:622` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `derive_exit_committed_completed_is_zero` | `crates/lattice-system-harness/src/session_send.rs:639` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `derive_exit_commit_failed_is_one` | `crates/lattice-system-harness/src/session_send.rs:656` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `derive_exit_turn_failed_is_one` | `crates/lattice-system-harness/src/session_send.rs:667` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `derive_exit_committed_error_status_is_one` | `crates/lattice-system-harness/src/session_send.rs:678` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `derive_exit_no_terminal_is_one` | `crates/lattice-system-harness/src/session_send.rs:686` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_send_stdout_concatenates_text_and_classifies` | `crates/lattice-system-harness/src/session_send.rs:697` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `parse_send_stdout_with_unknown_variant_tolerated` | `crates/lattice-system-harness/src/session_send.rs:728` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `assert_event_subsequence_satisfied_in_order` | `crates/lattice-system-harness/src/session_send.rs:742` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `assert_event_subsequence_missing_event_reports` | `crates/lattice-system-harness/src/session_send.rs:764` | 保留 | 局部不变量 | — | system harness 夹具/driver/隔离/失败产物/边界守卫局部契约；保留 |
| `fast_home_xdg_isolated_from_developer_home` | `crates/lattice-system-harness/tests/fast_cli_help.rs:21` | 保留 | 快速二进制黑盒 | — | 已落地快速二进制黑盒场景；保留为成品层载体 |
| `fast_cli_help_lists_public_commands` | `crates/lattice-system-harness/tests/fast_cli_help.rs:34` | 保留 | 快速二进制黑盒 | — | 已落地快速二进制黑盒场景；保留为成品层载体 |
| `fast_bare_lattice_prints_help_and_exits_zero` | `crates/lattice-system-harness/tests/fast_cli_help.rs:51` | 保留 | 快速二进制黑盒 | — | 已落地快速二进制黑盒场景；保留为成品层载体 |
| `fast_unknown_command_rejected` | `crates/lattice-system-harness/tests/fast_cli_help.rs:63` | 保留 | 快速二进制黑盒 | — | 已落地快速二进制黑盒场景；保留为成品层载体 |
| `msb_plain_text_turn_commits_and_exit_zero` | `crates/lattice-system-harness/tests/direct_local_turn.rs:108` | 提升 | Direct-Local 外部二进制 | `direct_local_plain_text_turn_commits_and_exit_zero` | #40 已落地 ordinary-lane Direct-Local plain-text turn scenario `direct_local_plain_text_turn_commits_and_exit_zero`；原 harness identity 由该 scenario 精确接管，decision 由 keep 改 elevate。 |
| `pty_tui_smoke_runs_turn_and_quits_cleanly` | `crates/lattice-system-harness/tests/pty_tui_smoke.rs:84` | 保留 | Direct-Local 外部二进制 | — | #40 后为 ordinary-lane Direct-Local PTY smoke；保留。 |
### `lattice-tui`（保留 3 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `new_session_project_branch_is_required` | `crates/lattice-tui/src/runtime.rs:405` | 保留 | 局部不变量 | — | TUI 局部纯逻辑（若有）保留；展示细节不进契约 |
| `new_session_project_branch_rejects_blank_values` | `crates/lattice-tui/src/runtime.rs:415` | 保留 | 局部不变量 | — | TUI 局部纯逻辑（若有）保留；展示细节不进契约 |
| `new_session_project_branch_preserves_explicit_value` | `crates/lattice-tui/src/runtime.rs:425` | 保留 | 局部不变量 | — | TUI 局部纯逻辑（若有）保留；展示细节不进契约 |

### `lattice-workspace`（保留 158 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `blocked_maps_to_blocked_plan` | `crates/lattice-workspace/src/lib.rs:735` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `open_maps_to_open_plan` | `crates/lattice-workspace/src/lib.rs:741` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `allowlist_propagates_domains_in_order` | `crates/lattice-workspace/src/lib.rs:746` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `allowlist_empty_is_empty_allow` | `crates/lattice-workspace/src/lib.rs:763` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `validate_baseline_oid_accepts_sha1_40_hex` | `crates/lattice-workspace/src/lib.rs:778` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `validate_baseline_oid_accepts_sha256_64_hex` | `crates/lattice-workspace/src/lib.rs:783` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `validate_baseline_oid_rejects_non_hex` | `crates/lattice-workspace/src/lib.rs:789` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `validate_baseline_oid_rejects_wrong_lengths` | `crates/lattice-workspace/src/lib.rs:795` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_create_upper_returns_handle` | `crates/lattice-workspace/src/testing/mod.rs:524` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_exec_upper_echo` | `crates/lattice-workspace/src/testing/mod.rs:537` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_destroy_is_idempotent` | `crates/lattice-workspace/src/testing/mod.rs:548` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_requested_policy` | `crates/lattice-workspace/src/testing/mod.rs:559` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_git_status_clean` | `crates/lattice-workspace/src/testing/mod.rs:573` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_deterministic_contract_suite` | `crates/lattice-workspace/tests/contract_deterministic.rs:21` | 保留 | 局部不变量 | — | DeterministicWorkspace adapter 合同套件入口；保留（workspace adapter 合同车道） |
| `test_deterministic_policy_is_recorded` | `crates/lattice-workspace/tests/contract_deterministic.rs:29` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_no_lower_path_in_handle_explicit` | `crates/lattice-workspace/tests/contract_deterministic.rs:66` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `env_var_debug_does_not_contain_value` | `crates/lattice-workspace/tests/cred_redaction.rs:15` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `file_cred_debug_does_not_contain_content` | `crates/lattice-workspace/tests/cred_redaction.rs:36` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `upper_spec_debug_via_credential_list` | `crates/lattice-workspace/tests/cred_redaction.rs:57` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_cred_debug_redaction` | `crates/lattice-workspace/tests/cred_redaction.rs:90` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_egress_variants_constructible` | `crates/lattice-workspace/tests/egress_policy.rs:25` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_egress_blocked_debug_is_printable` | `crates/lattice-workspace/tests/egress_policy.rs:48` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_egress_allowlist_entry_target` | `crates/lattice-workspace/tests/egress_policy.rs:55` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `allowlist_entries_are_publicly_constructible` | `crates/lattice-workspace/tests/egress_policy.rs:63` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `blocked_variant_is_publicly_constructible` | `crates/lattice-workspace/tests/egress_policy.rs:75` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `open_variant_is_publicly_constructible` | `crates/lattice-workspace/tests/egress_policy.rs:84` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `allowlist_entry_target_field_is_readable` | `crates/lattice-workspace/tests/egress_policy.rs:93` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `empty_bytes_are_clean` | `crates/lattice-workspace/tests/git_status_z.rs:16` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `ordinary_modified_entry` | `crates/lattice-workspace/tests/git_status_z.rs:25` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `ordinary_staged_added_entry` | `crates/lattice-workspace/tests/git_status_z.rs:38` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `untracked_entry` | `crates/lattice-workspace/tests/git_status_z.rs:49` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `multiple_ordinary_entries` | `crates/lattice-workspace/tests/git_status_z.rs:60` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `path_with_spaces_preserved_exactly` | `crates/lattice-workspace/tests/git_status_z.rs:73` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `path_with_embedded_newline_preserved_exactly` | `crates/lattice-workspace/tests/git_status_z.rs:81` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `path_with_tabs_and_special_whitespace` | `crates/lattice-workspace/tests/git_status_z.rs:89` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rename_entry_preserves_source_and_destination` | `crates/lattice-workspace/tests/git_status_z.rs:99` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `copy_entry_preserves_source_and_destination` | `crates/lattice-workspace/tests/git_status_z.rs:114` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rename_with_worktree_modification` | `crates/lattice-workspace/tests/git_status_z.rs:129` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rename_followed_by_ordinary_entry` | `crates/lattice-workspace/tests/git_status_z.rs:140` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `all_index_status_codes` | `crates/lattice-workspace/tests/git_status_z.rs:154` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `all_worktree_status_codes` | `crates/lattice-workspace/tests/git_status_z.rs:195` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `truncated_entry_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:217` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `truncated_rename_missing_second_path_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:228` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `non_utf8_path_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:239` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `absolute_path_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:252` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `traversal_path_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:263` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `invalid_status_code_rejected` | `crates/lattice-workspace/tests/git_status_z.rs:274` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `workspace_dirty_fact_is_valid_json` | `crates/lattice-workspace/tests/git_status_z.rs:287` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `workspace_dirty_fact_escapes_hostile_paths` | `crates/lattice-workspace/tests/git_status_z.rs:316` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `workspace_dirty_fact_cannot_inject_record` | `crates/lattice-workspace/tests/git_status_z.rs:333` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `crate_level_git_status_has_typed_entries` | `crates/lattice-workspace/tests/git_status_z.rs:353` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `git_status_default_is_clean_with_empty_entries` | `crates/lattice-workspace/tests/git_status_z.rs:370` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `changed_paths_from_typed_entries` | `crates/lattice-workspace/tests/git_status_z.rs:380` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `dirty_fact_from_parsed_output` | `crates/lattice-workspace/tests/git_status_z.rs:408` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_template_create_and_restore` | `crates/lattice-workspace/tests/integration_microsandbox.rs:118` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_egress_blocked_live` | `crates/lattice-workspace/tests/integration_microsandbox.rs:234` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_microsandbox_contract_suite` | `crates/lattice-workspace/tests/integration_microsandbox.rs:305` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_git_ready_template_has_git_under_blocked_egress` | `crates/lattice-workspace/tests/integration_microsandbox.rs:351` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `clean_export_preserves_candidate_oid` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:112` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `dirty_staged_rejects_export` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:158` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `dirty_unstaged_rejects_export` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:193` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `dirty_untracked_rejects_export` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:215` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `artifact_byte_limit_boundary` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:237` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_mismatch_rejects_before_import` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:454` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `failed_verification_rejects` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:476` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `valid_candidate_passes_validation` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:492` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `malformed_bundle_rejects` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:518` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `object_count_limit_boundary` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:540` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `gitmodules_change_rejects` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:594` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `symlink_add_rejects` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:645` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejection_leaves_refs_unchanged` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:692` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_empty_argv_rejected` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:721` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_empty_profile_hash_rejected` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:739` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_coherence_exit_mismatch_rejected` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:757` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_timeout_cannot_pass` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:776` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_valid_evidence_constructs` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:795` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_failed_with_nonzero_exit_constructs` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:814` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `verification_failed_with_timeout_constructs` | `crates/lattice-workspace/tests/promotion_candidate_validation.rs:830` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `prepare_rejection_creates_no_lock_or_journal` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:253` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `stale_project_ref_blocks_before_callback` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:264` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `dirty_lower_blocks_before_callback` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:286` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `callback_error_leaves_refs_and_lower_unchanged` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:304` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `existing_candidate_ref_with_same_oid_succeeds_via_verify` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:321` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `callback_uses_observed_session_parent_and_cas` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:336` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `absent_session_ref_uses_create_without_parent` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:360` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `lock_precedes_journal_and_lower_writes` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:373` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `clean_candidate_advances_refs_and_materializes_lower` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:395` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `noop_candidate_keeps_project_oid_while_session_and_candidate_refs_advance` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:416` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `post_ref_materialization_fault_requires_recovery_before_retry` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:458` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `third_state_conflict_preserves_journal` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:503` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `only_commit_and_recover_mutate_managed_lower` | `crates/lattice-workspace/tests/promotion_end_to_end.rs:529` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `deterministic_exact_baseline` | `crates/lattice-workspace/tests/promotion_exact_baseline.rs:72` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `deterministic_invalid_baseline_oid_rejected_before_handle` | `crates/lattice-workspace/tests/promotion_exact_baseline.rs:110` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `deterministic_missing_baseline_oid_rejected_before_handle` | `crates/lattice-workspace/tests/promotion_exact_baseline.rs:127` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `microsandbox_exact_baseline` | `crates/lattice-workspace/tests/promotion_exact_baseline.rs:151` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `provisions_linked_worktree_and_clean_baseline` | `crates/lattice-workspace/tests/promotion_lower.rs:94` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `reopens_existing_managed_worktree` | `crates/lattice-workspace/tests/promotion_lower.rs:109` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_branch_already_attached_elsewhere` | `crates/lattice-workspace/tests/promotion_lower.rs:119` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_staged_changes` | `crates/lattice-workspace/tests/promotion_lower.rs:130` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_unstaged_changes` | `crates/lattice-workspace/tests/promotion_lower.rs:147` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_untracked_files` | `crates/lattice-workspace/tests/promotion_lower.rs:159` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `allows_ignored_untracked_files` | `crates/lattice-workspace/tests/promotion_lower.rs:171` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_detached_head` | `crates/lattice-workspace/tests/promotion_lower.rs:193` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_wrong_branch` | `crates/lattice-workspace/tests/promotion_lower.rs:210` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_index_dirty_unmerged` | `crates/lattice-workspace/tests/promotion_lower.rs:235` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `same_identity_is_live_same_pid_different_ticks_is_stale` | `crates/lattice-workspace/tests/promotion_lower.rs:332` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `acquire_and_drop_records_parent_fsync` | `crates/lattice-workspace/tests/promotion_lower.rs:354` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `recovery_callback_failure_leaves_stale_lock` | `crates/lattice-workspace/tests/promotion_lower.rs:372` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `successful_recovery_removes_stale_lock` | `crates/lattice-workspace/tests/promotion_lower.rs:401` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `malformed_lock_json_is_not_auto_deleted` | `crates/lattice-workspace/tests/promotion_lower.rs:424` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `pin_creates_candidate_ref_with_exact_oid` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:111` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `pin_is_idempotent_for_same_oid` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:131` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `pin_rejects_different_oid` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:151` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `load_pinned_candidate_validates_and_returns` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:183` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `load_pinned_rejects_missing_ref` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:206` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `candidate_remains_resolvable_after_pin` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:225` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `prepare_pinned_rebuilds_validated_candidate` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:247` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `prepare_pinned_rejects_verification_mismatch` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:282` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `prepare_pinned_runs_hazard_and_limit_checks` | `crates/lattice-workspace/tests/promotion_pending_candidate.rs:319` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `add_modify_delete_and_executable_transitions_converge` | `crates/lattice-workspace/tests/promotion_recovery.rs:124` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `non_empty_directory_blocks_removal` | `crates/lattice-workspace/tests/promotion_recovery.rs:178` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `third_state_returns_conflict_without_mutation` | `crates/lattice-workspace/tests/promotion_recovery.rs:212` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `durable_fs_syncs_file_before_parent` | `crates/lattice-workspace/tests/promotion_recovery.rs:252` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `crash_matrix_recovers_toward_candidate_ref` | `crates/lattice-workspace/tests/promotion_recovery.rs:330` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `recovery_after_baseline_ref_preserves_baseline` | `crates/lattice-workspace/tests/promotion_recovery.rs:378` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `third_ref_and_third_state_preserve_journal` | `crates/lattice-workspace/tests/promotion_recovery.rs:413` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `torn_final_line_is_tolerated_malformed_middle_is_fatal` | `crates/lattice-workspace/tests/promotion_recovery.rs:451` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `golden_payload_has_one_transaction_cycle_and_three_commands` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:124` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `first_session_commit_uses_create_not_update` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:164` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `injection_corpus_rejects_before_git_spawn` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:180` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `derived_refs_match_d07_layout` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:214` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `success_moves_all_three_refs_together` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:232` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `stale_project_oid_leaves_refs_unchanged` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:247` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `stale_session_oid_leaves_refs_unchanged` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:267` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `existing_candidate_ref_blocks_whole_transaction` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:291` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `project_ref_moves_between_request_and_execution` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:311` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `session_ref_moves_between_request_and_execution` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:334` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `child_failure_surfaces_typed_error` | `crates/lattice-workspace/tests/promotion_ref_transaction.rs:377` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_retain_preserves_dirty_state_and_git` | `crates/lattice-workspace/tests/retained_snapshot.rs:87` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_retain_preserves_clean_commits` | `crates/lattice-workspace/tests/retained_snapshot.rs:150` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_retain_removes_live_handle` | `crates/lattice-workspace/tests/retained_snapshot.rs:196` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_release_is_idempotent` | `crates/lattice-workspace/tests/retained_snapshot.rs:215` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_list_returns_exact_session_snapshots` | `crates/lattice-workspace/tests/retained_snapshot.rs:253` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_quota_refuses_at_limit` | `crates/lattice-workspace/tests/retained_snapshot.rs:299` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_invalid_name_rejected` | `crates/lattice-workspace/tests/retained_snapshot.rs:346` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_zero_quota_always_refuses` | `crates/lattice-workspace/tests/retained_snapshot.rs:371` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_retain_nonexistent_handle_fails` | `crates/lattice-workspace/tests/retained_snapshot.rs:385` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_restore_nonexistent_snapshot_fails` | `crates/lattice-workspace/tests/retained_snapshot.rs:398` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `test_microsandbox_retain_restore_release` | `crates/lattice-workspace/tests/retained_snapshot.rs:426` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `wave0_a1_sandbox_send_sync` | `crates/lattice-workspace/tests/wave0_sdk_probe.rs:48` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `wave0_a3_boot_and_exec` | `crates/lattice-workspace/tests/wave0_sdk_probe.rs:70` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `wave0_a4_snapshot_round_trip` | `crates/lattice-workspace/tests/wave0_sdk_probe.rs:131` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `wave0_a7_egress_allowlist` | `crates/lattice-workspace/tests/wave0_sdk_probe.rs:231` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `deterministic_primitives_contract` | `crates/lattice-workspace/tests/workspace_primitives.rs:28` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `rejects_invalid_paths` | `crates/lattice-workspace/tests/workspace_primitives.rs:101` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `content_too_large_before_write` | `crates/lattice-workspace/tests/workspace_primitives.rs:121` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `git_facts_without_bypass` | `crates/lattice-workspace/tests/workspace_primitives.rs:137` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |
| `create_overwrite_delete_intents` | `crates/lattice-workspace/tests/workspace_primitives.rs:154` | 保留 | 局部不变量 | — | workspace adapter 合同、路径 fail-closed、凭证/egress/baseline 局部不变量；保留（KVM live 仍为可选车道） |

### `net-guard`（保留 15 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `reject_file_scheme` | `crates/net-guard/src/lib.rs:250` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_userinfo` | `crates/net-guard/src/lib.rs:255` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_plain_http_remote` | `crates/net-guard/src/lib.rs:260` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_https_loopback_ipv4` | `crates/net-guard/src/lib.rs:265` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_https_loopback_ipv6` | `crates/net-guard/src/lib.rs:270` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_private_ipv4` | `crates/net-guard/src/lib.rs:275` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `allow_localhost_http` | `crates/net-guard/src/lib.rs:282` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `allow_localhost_https` | `crates/net-guard/src/lib.rs:287` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `allow_https_public` | `crates/net-guard/src/lib.rs:292` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_bad_scheme` | `crates/net-guard/src/lib.rs:298` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_empty_host_after_scheme` | `crates/net-guard/src/lib.rs:303` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_dns_rebinding_domains_in_base_url` | `crates/net-guard/src/lib.rs:308` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `resolve_public_socket_addrs_rejects_private_literal` | `crates/net-guard/src/lib.rs:329` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `resolve_public_socket_addrs_rejects_rebinding_domain` | `crates/net-guard/src/lib.rs:336` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |
| `reject_empty_url` | `crates/net-guard/src/lib.rs:343` | 保留 | 局部不变量 | — | net-guard 局部不变量（wire/安全/dispatch/投影）；保留 |

### `xtask`（保留 26 / 提升 0 / 删除 0）

| 测试 | 文件 | 决定 | 目标层 | 替代场景 | 理由 |
|---|---|---|---|---|---|
| `ordered_test_crates_is_deterministic_and_excludes_xtask` | `xtask/src/ci.rs:310` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `ordered_test_crates_puts_dependencies_before_dependents` | `xtask/src/ci.rs:340` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `flags_string_form_lattice_dependency` | `xtask/src/harness_boundary/check.rs:180` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `flags_package_rename_with_neutral_key` | `xtask/src/harness_boundary/check.rs:200` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `flags_single_quoted_path_under_crates` | `xtask/src/harness_boundary/check.rs:218` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `bin_path_does_not_false_positive` | `xtask/src/harness_boundary/check.rs:236` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `workspace_true_in_comment_still_detected` | `xtask/src/harness_boundary/check.rs:253` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `parent_relative_path_under_crates_is_flagged` | `xtask/src/harness_boundary/check.rs:266` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `transitive_workspace_member_hit_when_present` | `xtask/src/harness_boundary/check.rs:286` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `manifest_workspace_inheritance_clean_manifest_empty` | `xtask/src/harness_boundary/check.rs:334` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `transitive_workspace_members_clean_workspace_empty` | `xtask/src/harness_boundary/check.rs:358` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `allowlist_prefix_semantics` | `xtask/src/lancet_boundary/check.rs:149` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `filter_allowlist_drops_matching_prefix` | `xtask/src/lancet_boundary/check.rs:161` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `cli_scan_skips_tests_dir_and_inline_test_files` | `xtask/src/lancet_boundary/check.rs:173` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `cli_allowlist_run_tests_only` | `xtask/src/lancet_boundary/check.rs:189` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `lattice_agent_manifest_lancet_native_hit` | `xtask/src/lancet_boundary/check.rs:210` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `default_tool_definitions_allowlist_only_fixture` | `xtask/src/lancet_boundary/check.rs:243` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `composite_executor_allowlist_registry_only` | `xtask/src/lancet_boundary/check.rs:262` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `tool_executor_reexport_detected_in_agent` | `xtask/src/lancet_boundary/check.rs:282` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `tool_executor_reexport_clean_agent_empty` | `xtask/src/lancet_boundary/check.rs:292` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `default_tool_executor_scan_flags_non_allowlisted` | `xtask/src/lancet_boundary/check.rs:302` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `default_tool_definitions_allowlisted_fixture_dropped` | `xtask/src/lancet_boundary/check.rs:317` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `lattice_agent_clean_manifest_no_hit` | `xtask/src/lancet_boundary/check.rs:336` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `target_directory_excluded_from_scan` | `xtask/src/lancet_boundary/check.rs:356` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `planning_dir_excluded_from_scan` | `xtask/src/lancet_boundary/check.rs:366` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |
| `normpath_collapses_parent_dirs` | `xtask/src/path_norm.rs:26` | 保留 | 局部不变量 | — | xtask/CI 工具与 harness-boundary 局部契约；保留 |

---

## 3. 机器可读副本

同目录 `test-triage-decisions.json`：每条含 `crate` / `file` / `line` / `name` / `decision` / `target_layer` / `replacement` / `reason` / `ignored`。
4/6 可用脚本按 `decision` 过滤后分批改代码。

## 4. 验收对照（issue #22）

- [x] 清单覆盖枚举到的全部现存测试，无遗漏条目（`docs/test-triage-decisions.json` 共 1449 条，与枚举键集合一致）
- [x] 每条都有决定、理由、目标层；提升/删除条目标明替代场景（或显式 `n/a`）
- [x] 以文档形式入库（本文件 + JSON）
- [x] 本票未删除、未修改任何测试源码
