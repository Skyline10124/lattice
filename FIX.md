# FIX — Changelog

Applied fixes across code review phases. Entries are append-only; never delete or overwrite.

---

## M-11 修复: head_transcript 拓扑遍历 merge 父分支 — 2026-05-23

Verified: `cargo test -p lattice-session` ✓ (61 tests), `cargo clippy -p lattice-session -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| M-11 | MEDIUM | `head_transcript_impl` 只走 first-parent，merge 侧消息丢失 | `collect_reachable_commits_topological`（迭代 postorder，`parents_json` 顺序）；linear / merge / diamond 测试 |

文件: `lattice-session/src/sqlite_graph/helpers.rs`, `commit.rs`, `tests/head_transcript.rs`

---

## H-06 修复: production `stream_turn` live streaming — 2026-05-23

Verified: `cargo test -p lattice-api` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo test -p lattice-cli run_integration` ✓

| # | 严重度 | 问题 | 修复 | 文件 |
|---|--------|------|------|------|
| H-06 | HIGH | production `stream_turn` 执行完整个 turn 后 replay buffered events，调用方无法获得渐进 token 推送 | Phase 16：`stream_turn` → `stream_turn_live`（spawn + bounded channel，边跑边 emit）；Phase 17 failure/consistency 覆盖；Phase 18 CLI `run` 切到 public `stream_turn`；Steps 1–4：`run_turn` 改为 drain live stream + `RunTurnCollector`，删除 `TurnDeps` / buffered replay seam | `lattice-api/src/turn/live.rs`, `execute.rs`, `collector.rs` |

Steps 1–4 明细见下方 **Turn execution unification** 节。

---

## M-19a/b 修复: live 背压不阻塞 agent / commit — 2026-05-23

Verified: `cargo test -p lattice-agent --features test-utils` ✓, `cargo test -p lattice-api stream_turn` ✓, `cargo clippy -p lattice-agent -p lattice-api --features test-utils --all-targets -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 | 文件 |
|---|--------|------|------|------|
| M-19a | MEDIUM | `emit_agent_event` 在满 channel 上 `send().await` 阻塞 `AgentLoop`，cancel/timeout 无法推进 | live mirror 改为 `try_send`；`buffered` 保持完整 `AgentTurnResult.events` | `lattice-agent/src/agent_loop/mod.rs` |
| M-19b | MEDIUM | `turn_tx.send().await` 阻塞 live task，无法 drain `agent_rx`，与 agent 侧背压双向僵局 | `ObserverSink`：delta `try_send`；非 delta 500ms 超时后放弃 observer，仍 drain 并 commit | `lattice-api/src/turn/live.rs` |

---

## M-20 修复: 批量 IN 查询分块 — 2026-05-23

Verified: `cargo test -p lattice-session` ✓ (58 tests), `cargo clippy -p lattice-session -- -D warnings` ✓

| # | 严重度 | 问题 | 修复 |
|---|--------|------|------|
| M-20 | MEDIUM | `IN (...)` 占位符数量随 message/commit ID 增长，可超出 SQLite `SQLITE_MAX_VARIABLE_NUMBER` 限制（默认 999） | 三处查询（`log_impl` tool_set、`commit_message_ids_batch`、`load_graph_messages_by_id`）均按 `SQLITE_BIND_CHUNK_SIZE`（900）分块查询后合并结果；新增 `chunked_queries` 测试模块覆盖 930 参数跨块场景 |

---

## v1.2 安全修复 — 2026-05-23

Verified: `cargo test -p lattice-llm -p lattice-tools` ✓, `cargo clippy -p lattice-llm -p lattice-tools -- -D warnings` ✓

### MEDIUM

| # | 问题 | 修复 | 文件 |
|---|------|------|------|
| M-04 | `validate_base_url` DNS 检查与 reqwest 请求之间存在 TOCTOU | 请求前钉 IP（**PARTIAL**，见 FINDINGS M-04） | `lattice-llm/src/security.rs`, `lattice-llm/src/client.rs` |
| M-05 | `web_fetch` 同 M-04；rebinding 黑名单不完整 | 钉 IP + 已知后缀黑名单（**PARTIAL**，见 FINDINGS M-05） | `lattice-tools/src/executor/web.rs` |

---

## v1.2 前瞻修复 — 2026-05-22

Verified: `cargo test -p lattice-session -p lattice-api -p lattice-cli -p lattice-profile -p lattice-llm` pass

### MEDIUM

| # | 问题 | 修复 | 文件 |
|---|------|------|------|
| M-12 | `hash_json` 依赖 serde_json 序列化顺序 | 引入 `canonical_json` 递归排序 object keys | `lattice-session/src/graph.rs` |
| M-13 | profile `canonicalize` 失败时回退非规范路径 | 改为返回 `ProfileError::SystemPathNotFound` | `lattice-profile/src/load.rs` |
| M-15 | `map_session_error` 丢弃结构化错误信息 | 添加 Conflict/BadRequest/MissingCommit/Io/Database 映射 | `lattice-api/src/session.rs` |
| M-18-01 | release-gate.sh 缺 workspace crate | 添加 lattice-session/tools/profile/hooks | `scripts/release-gate.sh` |
| M-18-02 | `status_to_exit_code` 死代码 | 删除函数及 3 个遗留测试 | `lattice-cli/src/commands/run.rs` |

### LOW

| # | 问题 | 修复 | 文件 |
|---|------|------|------|
| L-02 | `parse_json_data` 用 sentinel 做控制流 | 改为 `Result<Option<Value>, LlmEvent>`，语义清晰 | `lattice-llm/src/providers/mod.rs` + 调用方 |
| L-18-02 | TurnEvent 序列化 `.expect()` panic | 改为 `map_err` + `?` | `lattice-cli/src/commands/run.rs` |
| L-18-03 | 无 stream 超时机制 | CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1；API/Agent durable deadline 留未来 | `lattice-cli/src/commands/run.rs` |

---

## v1.1 收口审查 — 2026-05-19

Verified: `cargo fmt --all --check` ✓, `cargo clippy --all-targets -- -D warnings` ✓, `cargo test` 427 pass ✓

### HIGH

| # | 问题 | 修复 | 分支 |
|---|------|------|------|
| H-01 | thread-local SSE parser 跨请求状态泄漏 | 每次 `stream_sse_response` 创建独立 `ProviderParser` 实例 | llm-stream-sse-hardening |
| H-02 | `stream_sse_response` 整体下载（假流式 + OOM） | `response.bytes_stream()` 逐 chunk 解析，边读边检查累计大小 | llm-stream-sse-hardening |
| H-03 | `write_file` 跟随 symlink 绕过 project-root **(partial)** | 写入前 `symlink_metadata` 检测并拒绝；残留 TOCTOU 窗口留 v1.2 | tool-safety-hotfix |
| H-04 | `truncate_output` 非 UTF-8 字符边界 panic | `is_char_boundary` 循环回退到最近合法边界 | tool-safety-hotfix |
| H-05 | bash 命令不设 `current_dir` | Unix/非 Unix 分支均添加 `.current_dir(project_root)` | tool-safety-hotfix |
| H-07 | `tool_output_is_ok` 字符串前缀判断成功/失败 | 统一 `ToolExecutor` trait 为 `Result<String, String>`，删除前缀判定和 adapter | agent-loop-hardening |
| H-08 | agent loop 无迭代上限 | `MAX_TURN_ROUNDS = 200` hard cap + 测试 | agent-loop-hardening |
| H-09 | SQLite 每次新建连接 + TOCTOU 竞态 | `Mutex<Option<Connection>>` 持久化单连接 + lazy init + WAL | sqlite-consistency |
| H-10 | `validate_agents_dir` canonicalize 回退原始路径 **(partial)** | 手动按 `Component` 规范化，`..` 越根报错；symlink 残留留 v1.2 | tool-safety-hotfix |

### MEDIUM

| # | 问题 | 修复 | 分支 |
|---|------|------|------|
| M-01 | OpenAI 硬编码 `stream: true` | 改为 `request.stream`，`stream_options` 条件附加 | llm-stream-sse-hardening |
| M-02 | Gemini 忽略 temperature/max_tokens | 添加 `generationConfig` 块 | llm-stream-sse-hardening |
| M-03 | `validate_base_url` 接受空字符串 | 改返回 `Err("base_url must not be empty")` | llm-stream-sse-hardening |
| M-06 | `grep` 不做前置 project-root 校验 | 入口处 canonicalize + `ensure_under_project_root` | tool-safety-hotfix |
| M-07 | `read_text_file` TOCTOU | 读取后 recheck 实际 content 长度 | tool-safety-hotfix |
| M-08 | blocking `is_file()`/`is_dir()` 在 async | 改用 `tokio::fs::metadata` | tool-safety-hotfix |
| M-09 | SQLite 无 WAL 模式 | `PRAGMA journal_mode = WAL; synchronous = NORMAL` | sqlite-consistency |
| M-10 | `unwrap_or_default()` 吞掉 corrupt JSON | `parse_json_column` helper 返回 `SessionError::Database` | sqlite-consistency |
| M-14 | `ForbiddenSection` 错误信息误导 | 只列出真正允许的 section | misc-fixes |
| M-16 | `TurnEvent` 死 variants | 删除 `Reasoning`/`ToolCallDelta`/`ToolCallEnd` | misc-fixes |
| M-17 | `topology.rs` 孤立死代码 | 删除文件 | misc-fixes |
| M-18 | `project_root` 回退空字符串 | 改回退 `"/"` | misc-fixes |

### LOW

| # | 问题 | 修复 | 分支 |
|---|------|------|------|
| L-01 | `u32_field` 静默截断 | `u32::try_from(...).unwrap_or(u32::MAX)` | llm-stream-sse-hardening |
| L-03 | `ToolCallEnd` 顺序不定 | `BTreeMap<u32, String>` 按 index 排序 | llm-stream-sse-hardening |
| L-04 | `display_id` byte 切片 | `char_indices().nth(12)` 安全截断 | misc-fixes |
| L-05 | `config_dir()` 回退相对路径 | `HOME/.config` fallback + panic on missing | misc-fixes |

### 重构

| 改动 | 内容 | 分支 |
|------|------|------|
| trait 统一 | `lattice_tools::ToolExecutor` = `lattice_agent::ToolCallExecutor`，签名 `Result<String, String>`，12 个工具函数返回 `Err` | agent-loop-hardening |
| 工具面简化 | 删除 `read_files`（并行 `read_file` 替代），`web_search` → `web_fetch` | tool-safety-hotfix |
| 测试去重 | `test_helpers` module 提取共享 `user_msg`/`assistant_msg`/`tool_msg` | misc-fixes |
| hooks 移除 | `lattice-hooks` 从 workspace members 移除（文件保留，不编译） | misc-fixes |

### Phase 11 遗留项

| # | 问题 | 修复 |
|---|------|------|
| WR-02 | env var 测试同步 | Router tests 用 local env mutex |
| WR-03 | SQLite per-method connection | 合并到 H-09 |
| WR-04 | tool_output_is_ok 前缀 | 合并到 H-07 |
| WR-06 | turn parent_commit_id TOCTOU | 初始 transcript 读取时 capture parent |
| IN-01 | 重复测试 helper | 提取到 `lattice_session::test_helpers` |
| IN-02 | lattice-hooks 未使用 | 从 workspace members 移除 |
| IN-03 | CLI 依赖 hooks 未 import | 删除依赖 |

### PR #6 遗留项

| # | 问题 | 修复 |
|---|------|------|
| P2-01 | 模型覆盖未持久化 | turn commit metadata 正确记录 override model |
| P2-02 | NULL-tip branch checkout 失败 | 区分 branch exists + NULL commit_id vs missing |
| P2-03 | detached HEAD 无法 branch | 从 head_commit_id 取 detached commit |
| P2-05 | branch move 不同步 HEAD | branch upsert 同步 `graph_sessions.head_commit_id` |

---

## Phase 11 — 2026-05-18

Verified: `cargo fmt --all --check` ✓, `cargo clippy -- -D warnings` ✓, `cargo test` 504 pass ✓

### CR-01: Unknown message roles silently mapped to User
- **File:** `crates/lattice-agent/src/agent_loop.rs:474-501`
- **Before:** `_` wildcard branch mapped unknown roles to `Role::User`, corrupting conversation
- **After:** `convert_to_llm_message` returns `Result<Message, String>`, errors on unknown roles; caller returns `AgentTurnStatus::Failed`(Deprecated)

### CR-02: Provider hardcoded "unknown" in TurnCommitInput
- **File:** `crates/lattice-api/src/turn.rs:501`
- **Before:** `provider: "unknown".to_string()` — audit trail destroyed
- **After:** `resolve_model_and_prompt` returns `(model, prompt, provider)`; provider resolved via `ModelRouter::resolve(&ModelTarget::new(&model))`

### CR-03: ${VAR} env var expansion not blocked in check_command
- **File:** `crates/lattice-tools/src/executor.rs:877`
- **Before:** Metacharacter list blocked `$(` but not `$` or `${`, allowing credential leakage via `echo ${ANTHROPIC_API_KEY}`
- **After:** `"${"` and `"$"` added to blocked metacharacter list

### CR-04: Literal API key stored as both HashMap key and value
- **File:** `crates/lattice-cli/src/credentials.rs:53`
- **Before:** `values.insert(key_spec.clone(), key_spec.clone())` — same secret as key and value
- **After:** `values.insert(format!("config:{}", provider_name), key_spec.clone())` — secret appears only as value

### CR-05: aliases() returns empty Vec when no credentials
- **File:** `crates/lattice-llm/src/router.rs:103-119`
- **Before:** Doc claimed aliases() filtered by credential status; code already returned all known aliases
- **After:** Doc updated — `resolve()` returns `Ok(ResolvedModel)` for all known aliases regardless of `CredentialStatus`

### WR-01: Blanket #![allow] suppresses all lint warnings
- **File:** `crates/lattice-cli/src/lib.rs:1`
- **Before:** `#![allow(dead_code, unused_imports, private_interfaces)]` — masked unused deps, dead code
- **After:** Removed; per-module `#![allow(dead_code)]` for staged command modules only

### WR-05: ProviderConfig api_key serializable without protection
- **File:** `crates/lattice-cli/src/config.rs:26`
- **Before:** `pub api_key: Option<String>` with `#[derive(Serialize)]` — literal keys leak on serialization
- **After:** `#[serde(skip)]` added to `api_key` field; Deserialize reads, Serialize omits

### WR-07: write_file uses string concatenation for path construction
- **File:** `crates/lattice-tools/src/executor.rs:491-495`
- **Before:** `format!("{}/{}", self.project_root, path.trim_start_matches('/'))` — fragile, platform-specific
- **After:** `PathBuf::from(&self.project_root).join(path.trim_start_matches('/'))`

---

## Phase 12 repair — 2026-05-21

Verified: `rustfmt --edition 2021 --check` on touched Rust files ✓, `cargo test -p lattice-api` ✓, `cargo test -p lattice-llm provider_kind_as_str_is_stable_snake_case` ✓, `cargo clippy -p lattice-api -p lattice-llm --tests -- -D warnings` ✓

| # | 问题 | 修复 |
|---|------|------|
| WR-01 | Resolved provider dropped before `AgentTurnRequest` | API now passes routed provider into `AgentTurnRequest.provider`, so `ModelStarted.provider` is populated on the API path |
| WR-04 | Provider string used `Debug` lowercase | Added `ProviderKind::as_str()` stable identifiers, including `openai_compatible` |
| WR-05 | `RealTurnDeps` cloned `AgentTurnResult.events` | Uses `std::mem::take` to move events out of the result without cloning |
| Planning | `STATE.md` said milestone complete; `VER-07` pending; `EVT-05` overbroad | Local `.planning/` state updated to Phase 13 ready, `VER-07` complete, and `EVT-05` narrowed to vocabulary representation |
| Deferred | WR-02/WR-03 need later semantic work | Local Phase 12 deferred-items file logs stage honesty for Phase 17 and timeout partial preservation for Phase 14/17 |

---

## Phase 14 follow-up — 2026-05-21

Verified: `cargo test -p lattice-agent --features test-utils --test streaming_delta` ✓, `cargo test -p lattice-agent --features test-utils` ✓, `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` ✓, `cargo clippy --workspace --all-targets -- -D warnings` ✓, `cargo fmt --all --check` ✓

| # | 问题 | 修复 |
|---|------|------|
| P1-14-01 | `ToolCallEnd` 后同 id 的 `ToolCallDelta` 仍可修改最终 arguments | `consume_model_stream` rejects delta-after-end and duplicate-end as invalid tool-call stream states; regression tests assert Error status, no execution, no `ModelFinished`, and no malformed committed tool calls |
| P1-14-02 | invalid tool-call-only response commits an empty assistant message | Invalid tool-call path now commits assistant content/reasoning only when either exists; regression test asserts malformed tool-only responses leave `messages` empty |
| Deferred | Phase 14 cancel checkpoints incomplete by design | `.planning/STATE.md` records Phase 15 cooperative cancellation checkpoints |

---

## Phase 17 review closeout — 2026-05-22

Verified: `cargo fmt --all --check` ✓, `cargo test -p lattice-agent --features test-utils timeout` ✓, `cargo test -p lattice-api stream_turn` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo clippy -p lattice-agent --features test-utils --all-targets -- -D warnings` ✓, `cargo clippy -p lattice-api --all-targets -- -D warnings` ✓, `cargo test --workspace` ✓

| # | 问题 | 修复 |
|---|------|------|
| P1-17-01 | Timeout cooperative deadline did not cover tool execution | `ToolCallExecutor::execute` now awaits through the deadline helper; timeout during a tool preserves state, emits no fake `ToolCallFinished`, and commits `Error(timeout)` metadata |
| P1-17-02 | Expired deadline could lose to ready stream events | Deadline expiry is checked before awaiting and deadline sleeps use `sleep_until`, so timeout wins once expired |
| P2-17-03 | Run/stream consistency did not compare committed terminal metadata | Both observation paths now read `terminal_reason` from committed log metadata; error stream events assert public `TurnFailed.reason` matches committed metadata |
| P2-17-04 | Run/stream consistency did not compare committed usage metadata | Both observation paths now read usage from committed log metadata and normalize zero usage |
| P2-17-05 | Receiver-drop test used fixed sleep | Test now polls for background commit completion with an outer timeout |
| Docs | Live stream and timeout contracts were stale | Rustdoc, Phase 17 validation/verification, summaries, roadmap wording, FINDINGS, and codebase docs now match the alpha evented-runtime design |
| Deferred | Live channel backpressure is outside cooperative timeout | Accepted alpha gap recorded in `.planning/STATE.md`; deadline-aware event emission is deferred until live/result event reliability semantics are specified |

---

## Turn execution unification (Steps 1–4) — 2026-05-23

Verified: `cargo test -p lattice-api` ✓, `cargo test -p lattice-api --test consistency` ✓, `cargo test -p lattice-api --test usage_turn` ✓, `cargo test -p lattice-cli run_integration` ✓, `cargo clippy -p lattice-api --all-targets -- -D warnings` ✓

| # | 问题 | 修复 |
|---|------|------|
| Step 1 | Production `run_turn` used buffered `TurnDeps` path parallel to live `stream_turn` | `run_turn` now drains live `stream_turn` via `RunTurnCollector`; round-aware `assistant_output`, usage sum, synthetic stages |
| Step 2 | `turn/tests.rs` depended on `FakeTurnDeps` buffered seam | Migrated to public `run_turn` / `stream_turn` with `SessionApi` + `FakeStreamingLlmClient`; added optional `RunTurnRequest.cancel_flag` for cooperative-cancel E2E |
| Step 3 | Legacy `TurnDeps` / `execute_turn_inner` / `run_turn_with_deps` / `stream_turn_with_deps` duplicated orchestration | Deleted `deps.rs`, buffered execute path, and `map_tool_results`; single core is `stream_turn_live` |
| Step 4 | Consistency tests compared two full execution paths with dual tempdirs | Rewrote to assert `run_turn` == `collect_turn_result_from_events(drain stream_turn)` plus durable session-store anchor |
