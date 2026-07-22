# #23 切片 3：退役 KVM-lane 词汇与 assertion-level #24 映射

- 状态：已完成（2026-07-19；ledger/prose 已落地，见 FIX.md #23 slice 3/4）
- 父计划：`2026-07-19-issue23-fast-residual-ledger-closeout.md` §3
- 前置：切片 1/2（fast residual 已归零）。
- 与切片 4 **同一 PR、相邻 commit**：本切片改 machine ledger，切片 4 立即同步 prose。
- 收口前精确分布：退役 KVM-lane `target_layer` 曾有 57 条 = elevate 55 + keep 2；
  其中 replacement 含退役 lane 后缀的是 55 条 elevate，2 条 keep 只有 layer 过时。**现已清零。**

## 3.1 先对账：存在性与预期 decision delta

执行前以当时 Rust 源重新跑（historical closeout script）。退役 KVM-lane `target_layer`
字面量不在本计划复写；现行 ledger 验收断言该类 layer 已为空：

```python
import json, pathlib, re
entries = json.load(open("docs/test-triage-decisions.json"))
# Historical closeout filtered the retired KVM-lane target_layer (token in FIX.md #23).
# After slice 3, that filter must be empty in active ledger:
retired = [e for e in entries if e.get("target_layer", "").startswith("real_")
          and "sand" in e.get("target_layer", "")]
assert retired == [], retired
rust = {p: p.read_text(errors="ignore") for p in pathlib.Path("crates").rglob("*.rs")}
# Pre-closeout inventory used len(retired)==57; do not re-apply that number to current JSON.
```

> 注：退役 token 的精确字面量只保留在 FIX.md / 已归档计划；本文件与 active ledger 验收以零命中为准。

已核验基线：55 条 elevate 中 44 present / 11 absent；2 条 keep 中
`msb_plain_text_turn_commits_and_exit_zero` absent、PTY smoke present。

预期 decision 计数变化（只有这些显式改判）：

- 11 条已随 #37–#40 删除的 Upper/promotion/prune 测试：elevate → delete；
- 历史 `msb_plain_text_turn_commits_and_exit_zero`：keep → elevate，精确指向已落地
  `direct_local_plain_text_turn_commits_and_exit_zero`；
- 其余 decision 不变。

若执行前源存在性或数量变化，停止并重算；不得套用旧数字。按当时基线，最终全局计数应为
**keep 1337 / elevate 86 / delete 26，合计 1449**。

## 3.2 已被 #40 场景精确接管（1 条历史 identity）

| ledger test identity | replacement scenario | 执行 |
| `msb_plain_text_turn_commits_and_exit_zero`（旧 harness，已不存在） | `direct_local_plain_text_turn_commits_and_exit_zero` | keep→elevate，更新 layer/reason |

源码逐断言复核确认现有 external scenario **不能** superset 接管以下两条，故不得删除：

- `stream_turn_live_with_agent_basic_completed` 额外断言 TurnStarted/TurnCommitting、无 failed/cancelled、
  started/committed session ID 相等 → `planned_direct_basic_completed_event_contract`；
- `run_integration_commits_existing_session` 额外断言 `RunTurnResult.assistant_output/status` →
  `planned_direct_existing_session_run_result`。

## 3.3 已退役、无 #24 场景的 11 条（elevate → delete）

这些 identity 在当时 source 已不存在，行为绑定已删除的 Upper/promotion/prune / 退役 KVM-lane surface：

```text
stale_verification_reaches_no_promotion_effects
success_uses_real_session_callback_and_materializes_lower
promotion_module_documents_activation_requirement
prune_one_session_does_not_remove_another
prune_idempotent_second_call_returns_zero_released
prune_session_snapshots_skips_pending_worktree_snapshot
cleanup_status_error_yields_error_aborted
upper_routing_e2e
adapter_failure_has_no_lower_fallback
generated_profiles_are_executable_host_only
<sandbox-product>_upper_routing
```

（末行历史 identity 前缀为已删 sandbox 产品名；此处不复写字段字面量。完整名单见 FIX.md #23。）

统一处理示例：

```json
{
  "decision": "delete",
  "target_layer": "local_invariant",
  "replacement": "n/a",
  "reason": "#37–#40 已删除该 Upper/promotion/KVM-lane 产品契约；对应测试已不存在，不向 #24 虚构 replacement。"
}
```

这里不是提前删除测试，而是把 #40 已发生的删除补记进 ledger。

## 3.4 #24 assertion-level scenario IDs（44 条；禁止 family fallback）

每个 ID 对应一个具体可执行 scenario/assertion bundle。只有断言形成 superset 时才允许多个
ledger identity 指向同一 ID；本表当前选择一行为一个精确 ID，避免旧三元/大类兜底。

| ledger test identity | exact #24 replacement scenario |
| `agent_loop_builds_messages_from_transcript_and_user_input` | `planned_direct_message_assembly` |
| `tool_execution_error_structured_payload_preserved` | `planned_direct_tool_error_payload` |
| `streaming_backward_compat_fake_llm_chat_only_still_works` | `planned_direct_streaming_chat_compat` |
| `stream_turn_live_with_agent_basic_completed` | `planned_direct_basic_completed_event_contract` |
| `stream_turn_live_with_agent_error_commits_error` | `planned_direct_live_error_commit` |
| `stream_turn_live_with_agent_event_ordering` | `planned_direct_live_event_order` |
| `stream_turn_live_with_agent_startup_failure_no_stream` | `planned_direct_provider_startup_failure` |
| `stream_turn_live_with_agent_commit_stores_delta_only` | `planned_direct_delta_only_commit` |
| `stream_turn_live_with_agent_branch_populated` | `planned_direct_branch_population` |
| `stream_turn_live_with_agent_commit_failure_deterministic` | `planned_direct_commit_failure` |
| `stream_turn_live_with_agent_receiver_drop_does_not_cancel` | `planned_direct_receiver_drop_commit` |
| `stream_turn_live_with_agent_slow_observer_still_commits` | `planned_direct_slow_observer_commit` |
| `stream_turn_emits_tool_activity_before_committed` | `planned_direct_tool_activity_before_commit` |
| `stream_turn_error_still_commits_error` | `planned_direct_stream_error_commit` |
| `stream_turn_cancelled_commits_cancelled` | `planned_direct_cancelled_commit` |
| `stream_turn_error_after_partial_commits_error` | `planned_direct_partial_output_error_commit` |
| `stream_turn_emits_cep_lifecycle_events` | `planned_direct_cep_lifecycle` |
| `full_turn_audit_rows_and_replay_fragment` | `planned_direct_turn_audit_replay` |
| `consistency_error_after_partial_token` | `planned_direct_partial_token_parity` |
| `consistency_tool_call_path` | `planned_direct_tool_call_parity` |
| `stream_turn_emits_activity_before_commit` | `planned_direct_activity_commit_order` |
| `consistency_completed_disposition_parity` | `planned_direct_completed_disposition_parity` |
| `consistency_error_disposition_parity` | `planned_direct_error_disposition_parity` |
| `usage_run_turn_can_read_workspace_file_with_default_profile` | `planned_direct_workspace_read_turn` |
| `usage_stream_turn_model_started_includes_routed_provider` | `planned_direct_routed_provider_event` |
| `usage_resume_session_includes_previous_turn_context` | `planned_direct_reopen_previous_context` |
| `public_https_policy_requires_fetch_capability` | `planned_direct_fetch_capability_denial` |
| `rejects_profile_disabled_web_fetch` | `planned_direct_disabled_fetch_denial` |
| `audit_query_prints_fragment_for_seeded_db` | `planned_direct_audit_query_seeded` |
| `audit_tail_streams_rows_for_seeded_db` | `planned_direct_audit_tail_seeded` |
| `audit_query_empty_result_for_unknown_session` | `planned_direct_audit_query_unknown_session` |
| `audit_replay_reconstructs_turn_trace_for_seeded_db` | `planned_direct_audit_replay_trace` |
| `audit_replay_reports_stored_attestation_match` | `planned_direct_audit_attestation_match` |
| `audit_replay_errors_for_unknown_turn` | `planned_direct_audit_replay_unknown_turn` |
| `cep_status_seeded_audit_json` | `planned_direct_lancet_status_seeded` |
| `stream_timeout_fires_after_duration` | `planned_direct_provider_timeout` |
| `run_integration_commits_existing_session` | `planned_direct_existing_session_run_result` |
| `tool_executor_uses_workspace_root_not_profile_package_dir` | `planned_direct_canonical_workspace_root` |
| `branch_listing_reads_only_addressed_session_refs` | `planned_direct_branch_listing_isolation` |
| `checkout_changes_only_addressed_session_head` | `planned_direct_checkout_isolation` |
| `branch_create_affects_only_addressed_session` | `planned_direct_branch_create_isolation` |
| `reset_changes_only_addressed_session_branch_pointer` | `planned_direct_reset_isolation` |
| `diff_resolves_refs_inside_addressed_session_only` | `planned_direct_diff_isolation` |
| `smoke_session_log_shows_seeded_turn` | `planned_direct_session_log_turn` |

这也消解了旧的 resume-lane planned ID：commit failure 与 previous-context reopen 分别落到两个真实行为 ID，
不再假设已退役的 `session resume` CLI。

## 3.5 唯一允许的机械脚本形状

实现时把 §3.2/§3.4 表转成显式 `EXACT` dict，把 §3.3 转成 `OBSOLETE` set；不做
「退役 lane replacement → `planned_*_direct`」正则大类替换：

```python
import collections
# Example excerpt: before execution, transcribe all 45 rows from §3.2/§3.4.
EXACT = {
    "agent_loop_builds_messages_from_transcript_and_user_input":
        "planned_direct_message_assembly",
    "tool_execution_error_structured_payload_preserved":
        "planned_direct_tool_error_payload",
}
OBSOLETE = {
    "stale_verification_reaches_no_promotion_effects",
    "success_uses_real_session_callback_and_materializes_lower",
    "promotion_module_documents_activation_requirement",
    "prune_one_session_does_not_remove_another",
    "prune_idempotent_second_call_returns_zero_released",
    "prune_session_snapshots_skips_pending_worktree_snapshot",
    "cleanup_status_error_yields_error_aborted",
    "upper_routing_e2e",
    "adapter_failure_has_no_lower_fallback",
    "generated_profiles_are_executable_host_only",
    # plus the retired sandbox-product upper-routing identity (see FIX.md #23)
}
assert len(EXACT) == 45, "expand the excerpt from the authoritative tables"

# Historical closeout selected rows by the retired KVM-lane target_layer (see FIX.md #23).
rows = [e for e in entries
        if e.get("target_layer", "").startswith("real_") and "sand" in e.get("target_layer", "")]
elevate_names = {e["name"] for e in rows if e["decision"] == "elevate"}
assert set(EXACT) | OBSOLETE == elevate_names | {"msb_plain_text_turn_commits_and_exit_zero"}

for e in rows:
    e["target_layer"] = "direct_local_binary"
    if e["name"] == "msb_plain_text_turn_commits_and_exit_zero":
        e["decision"] = "elevate"
    if e["name"] in EXACT:
        e["replacement"] = EXACT[e["name"]]
    elif e["name"] in OBSOLETE:
        e.update(decision="delete", target_layer="local_invariant", replacement="n/a")
    elif e["name"] == "pty_tui_smoke_runs_turn_and_quits_cleanly":
        e["reason"] = "#40 后为 ordinary-lane Direct-Local PTY smoke；保留。"
    else:
        raise AssertionError(f"unmapped retired-lane ledger row: {e['name']}")

final = collections.Counter(e["decision"] for e in entries)
assert final == {"keep": 1337, "elevate": 86, "delete": 26}
```

另存 before/after 的 `crate/file/line/name/async/ignored`，断言完全一致；decision 只允许 §3.1 列出的 12 条变化。

## 3.6 assertion-superset 对照模板

PR 描述逐条列所有实际删除（包括切片 1/2）：

```text
<identity> | 内部断言集合 | replacement 场景断言集合 | superset? | 删除/保留
```

**红线：场景名相似不算覆盖；未通过 replacement 的原测试不删。**

## 验收

- JSON 中退役 KVM-lane `target_layer`、全 turn 退役 scenario 名、带退役 lane 后缀的 planned ID 为零；
  历史 identity 名 `msb_plain_text_turn_commits_and_exit_zero` 可保留（identity 不改）。
- 1449 条与 identity 字段不变；decision delta 恰为 §3.1。
- 44 个 planned ID 均精确、无 broad family fallback；切片 4 scenario catalog 全量登记。
- focused crate tests + workspace tests 通过；立即执行切片 4，不单独 merge。
