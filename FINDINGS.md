# FINDINGS — Changelog

Open findings across code review phases. Entries are append-only; mark resolved with `[resolved]` tag and date, never delete.

---

## v1.1 收口审查 — 2026-05-19

全量审查：8 crate，79 .rs 文件，21,600 行代码。

### HIGH — 必须修复

#### H-01: thread-local SSE parser 跨请求状态泄漏 — `correctness`
- **位置**: `crates/lattice-llm/src/client.rs:162`
- **问题**: `parse_provider_sse` 使用 `thread_local!` 静态 parser，tokio 线程复用时上一次请求的 `tool_call_ids`/`tc_counter`/`input_tokens` 残留，污染下一次 stream 解析。会导致 `ToolCallEnd` 事件发给错误的 tool call ID。
- **影响**: 多请求场景下流式解析结果错误（生产必现）
- **修复**: 每次 `stream_sse_response` 调用创建新 parser 实例，不使用 thread-local。

#### H-02: `stream_sse_response` 整体下载后才解析（假流式 + OOM） — `security`
- **位置**: `crates/lattice-llm/src/client.rs:93-132`
- **问题**: `response.bytes().await` 把整个响应体拉入内存后再做 SSE 解析。`MAX_SSE_BUFFER_SIZE`（2MB）检查在下载完成之后。恶意 provider 可发送数 GB 响应导致 OOM。
- **影响**: 内存耗尽 / DoS
- **修复**: 使用 `response.bytes_stream()` 逐 chunk 解析，边读边检查累计大小。

#### H-03: `write_file` 跟随符号链接绕过 project-root 限制 — `security`
- **位置**: `crates/lattice-tools/src/executor/fs.rs:143-171`
- **问题**: 只 canonicalize 父目录后 join 文件名，若目标文件是已存在的 symlink（如 `project/link.txt -> /etc/cron.d/job`），`ensure_under_project_root` 检查通过，但 `tokio::fs::write` 跟随 symlink 写到外部路径。对比 `read_text_file` 正确 canonicalize 了完整路径。
- **影响**: 任意文件写入
- **修复**: 写入前检查目标是否为 symlink；或 canonicalize 完整路径后再做边界校验。

#### H-04: `truncate_output` 在非 UTF-8 字符边界 panic — `correctness`
- **位置**: `crates/lattice-tools/src/executor/render.rs:101`
- **问题**: `String::truncate(limit)` 若 limit 落在多字节 UTF-8 字符中间则 panic。`from_utf8_lossy` 产生的 U+FFFD 是 3 字节，`MAX_COMMAND_OUTPUT_SIZE` 可能恰好落在其中间。
- **影响**: bash 工具输出含非 ASCII 时进程崩溃
- **修复**: 回退到最近的 char boundary。

#### H-05: bash 命令不设 `current_dir` — `correctness`
- **位置**: `crates/lattice-tools/src/executor/shell.rs:52-92`
- **问题**: `_project_root` 参数带下划线被显式忽略，`Command` 未调用 `.current_dir()`。所有 bash 工具调用在 lattice 进程的启动目录执行，而非项目根目录。
- **影响**: 工具契约违反，命令操作错误目录
- **修复**: 去掉下划线，添加 `.current_dir(project_root)`。

#### H-06: `stream_turn` 不是真正的流式 — `streaming`
- **位置**: `crates/lattice-api/src/turn/execute.rs:133-173`
- **问题**: `stream_turn_with_deps` 内部调用 `execute_turn_inner` 把整个 turn 执行完毕后用 `stream::iter(turn_events)` 一次性 replay。调用方无法获得渐进 token 推送。
- **影响**: streaming API 无法实现其设计目的
- **修复**: 需将 AgentLoop 改为 channel-based 事件推送架构（建议 v1.2）。
- **关联**: P2-04

#### H-07: `tool_output_is_ok` 用字符串前缀判断成功/失败 — `correctness` [resolved 2026-05-19]
- **修复**: 统一 `lattice_tools::ToolExecutor` 和 `lattice_agent::ToolCallExecutor` 为单一 trait，签名 `Result<String, String>`。`DefaultToolExecutor` 内部 12 个工具函数全部改为返回 `Err(...)` 而非 `format!("Error: ...")`。`tool_output_is_ok` helper 和 CLI 的 `classify_tool_output`/`ToolExecutorAdapter` 全部删除。`DefaultToolExecutor` 直接传给 `AgentLoopConfig.tool_executor`，agent loop 通过 `Ok`/`Err` 模式匹配判定 ok。
- **关联**: WR-04

#### H-08: agent loop 无任何迭代上限 — `resource-exhaustion` [resolved 2026-05-19]
- **修复**: 添加 `MAX_TURN_ROUNDS = 200` hard cap，超限返回 `Failed` + `"exceeded max rounds"` terminal_reason，并 emit Error event。新增测试验证无限 tool_call 场景下精确停在 200 轮。

#### H-09: SQLite 每次操作新建连接 + TOCTOU 竞态 — `race-condition`
- **位置**: `crates/lattice-session/src/sqlite_graph.rs:43-138`
- **问题**: 每次方法调用新建连接并执行完整 DDL。trait 要求 `Send + Sync`，两个线程并发 `commit_turn` 时各自读到相同 HEAD、都通过 parent 校验，可能产生图分叉。
- **影响**: 并发下会话图完整性受损
- **修复**: 连接池或 Mutex 保护单连接 + parent 校验放入事务内。
- **关联**: WR-03

#### H-10: `validate_agents_dir` canonicalize 失败时回退原始路径 — `security`
- **位置**: `crates/lattice-cli/src/commands/run.rs:252-278`
- **问题**: 路径不存在时 `canonicalize()` 失败，回退到未规范化的原始路径。`LATTICE_AGENTS_DIR=/home/../etc/cron.d` 原始字符串以 `/home` 开头通过验证，实际解析到 `/etc/cron.d`。
- **影响**: agents 目录验证可绕过
- **修复**: canonicalize 失败时返回错误而非回退。

### MEDIUM — 建议修复

#### M-01: OpenAI provider 硬编码 `"stream": true` — `correctness`
- **位置**: `crates/lattice-llm/src/providers/openai.rs:14-18`
- **问题**: JSON body 中 `"stream"` 硬编码为 `true`，忽略 `request.stream`。非流式模式不可用。

#### M-02: Gemini provider 忽略 `temperature` / `max_tokens` — `correctness`
- **位置**: `crates/lattice-llm/src/providers/gemini.rs:11-101`
- **问题**: `build_request_json` 从不读取这两个字段。OpenAI/Anthropic 都正确处理。

#### M-03: `validate_base_url` 接受空字符串 — `security`
- **位置**: `crates/lattice-llm/src/security.rs:52-55`
- **问题**: 空 URL 直接 `return Ok(())`，绕过全部 SSRF 检查。

#### M-04: DNS rebinding TOCTOU（lattice-llm） — `security`
- **位置**: `crates/lattice-llm/src/security.rs:100-117`
- **问题**: 验证时 DNS 解析一次检查非内网 IP，reqwest 发请求时再次解析。攻击者可在两次解析间切换到内网 IP。

#### M-05: DNS rebinding TOCTOU（lattice-tools）+ 不完整黑名单 — `security`
- **位置**: `crates/lattice-tools/src/executor/web.rs:12-15, 60-88`
- **问题**: 同 M-04。rebinding 域名黑名单只有 `nip.io`/`xip.io`，缺少多个已知域名。

#### M-06: `grep` 不做前置 project-root 校验 — `security`
- **位置**: `crates/lattice-tools/src/executor/search.rs:12-45`
- **问题**: 绝对路径如 `/tmp/sensitive` 通过 `reject_parent_traversal`，在递归函数内静默过滤返回 "(no matches)" 而非报错。

#### M-07: `read_text_file` TOCTOU — `race-condition`
- **位置**: `crates/lattice-tools/src/executor/fs.rs:38-55`
- **问题**: 先检查 metadata size 再读文件，期间文件可能增长超限。

#### M-08: blocking `is_file()`/`is_dir()` 在 async 上下文 — `performance`
- **位置**: `crates/lattice-tools/src/executor/search.rs:158, 219, 245`
- **问题**: 同步 `stat` 调用在 async 函数中，网络文件系统下阻塞 tokio worker thread。

#### M-09: SQLite 无 WAL 模式 — `performance`
- **位置**: `crates/lattice-session/src/sqlite_graph.rs:49-114`
- **问题**: 默认 rollback journal 模式下读写互斥，并发操作报 SQLITE_BUSY。

#### M-10: `unwrap_or_default()` 吞掉 corrupt JSON — `data-integrity`
- **位置**: `crates/lattice-session/src/sqlite_graph.rs:236, 514, 516, 1179, 1245, 1247`
- **问题**: 损坏 JSON 被静默当空数组，commit 看起来像 root commit，消息丢失。

#### M-11: `head_transcript` 只走 first-parent — `correctness`
- **位置**: `crates/lattice-session/src/sqlite_graph.rs:219`
- **问题**: 合并后的分支消息永远不会出现在 transcript 重建结果中。

#### M-12: `hash_json` 依赖 serde_json 序列化顺序 — `compatibility`
- **位置**: `crates/lattice-session/src/graph.rs:329-336`
- **问题**: commit ID 由序列化结果哈希得到，若 `preserve_order` feature 变化所有已有 ID 失效。

#### M-13: profile `canonicalize` 失败时回退非规范路径 — `security`
- **位置**: `crates/lattice-profile/src/load.rs:148-149`
- **问题**: 失败后 `starts_with` 比较 canonical vs non-canonical 路径，可能绕过限制。

#### M-14: `ForbiddenSection` 错误信息把 forbidden section 列为 "allowed" — `ux`
- **位置**: `crates/lattice-profile/src/error.rs:22`
- **问题**: 错误消息误导用户添加实际被禁止的 section。

#### M-15: `map_session_error` 丢弃结构化错误信息 — `observability`
- **位置**: `crates/lattice-api/src/session.rs:131-136`
- **问题**: 所有非 NotFound 的 SessionError 扁平化为 `ApiError::Internal(string)`。

#### M-16: `TurnEvent` 有永远不会被构造的 variants — `dead-code`
- **位置**: `crates/lattice-api/src/turn/types.rs:119-131`
- **问题**: `Reasoning`、`ToolCallDelta`、`ToolCallEnd` 永远不被产生，误导 API 消费者。

#### M-17: `topology.rs` 孤立死代码 — `dead-code`
- **位置**: `crates/lattice-api/src/topology.rs`（113行）
- **问题**: 未在 `lib.rs` 声明为 mod，从不编译。

#### M-18: `project_root` 静默回退空字符串 — `security`
- **位置**: `crates/lattice-api/src/turn/mapping.rs:56-64`
- **问题**: 无 profile 且 `current_dir()` 失败时 project_root 为 ""，可能禁用工具路径限制。

### LOW — 可选清理

#### L-01: `u32_field` 静默截断大值 — `correctness`
- **位置**: `crates/lattice-llm/src/providers/mod.rs:47-49`

#### L-02: `parse_json_data` 用 `Err(LlmEvent::Started)` 做 sentinel — `api-design`
- **位置**: `crates/lattice-llm/src/providers/mod.rs:14-22`

#### L-03: `ToolCallEnd` 事件顺序不确定 — `correctness`
- **位置**: `crates/lattice-llm/src/providers/openai.rs:195`

#### L-04: `display_id` byte 切片可能 panic — `correctness`
- **位置**: `crates/lattice-cli/src/commands/session.rs:387-393`

#### L-05: `config_dir()` 失败时 session 存到相对路径 — `ux`
- **位置**: `crates/lattice-cli/src/commands/run.rs:143-148`

### 优先级建议

**v1.1 release 前必修（安全/正确性）：**
1. H-03 write_file symlink
2. H-05 bash current_dir
3. H-04 truncate_output panic
4. H-01 parser 状态泄漏
5. H-10 agents_dir 验证绕过
6. H-07 tool_output_is_ok
7. H-08 无限循环保护

**release 前建议修（可靠性）：**
8. H-09 SQLite 并发
9. H-02 假流式 OOM
10. M-14 错误信息
11. M-01 OpenAI stream 硬编码
12. M-13 profile canonicalize

**v1.2 处理：**
- H-06 真流式（需改 AgentLoop 架构）
- M-04/M-05 DNS rebinding（需 reqwest resolve pinning）
- M-12 hash 稳定性（需引入 canonical JSON）
- M-11 transcript 合并遍历（需重设计 graph walk）

---

## Phase 11 — 2026-05-18

7 findings tracked across hardening. Some are now resolved; unresolved architectural items remain deferred.

### WR-02: Inconsistent env var test synchronization — `flaky-ci` [resolved 2026-05-19]
- **Files:** `crates/lattice-llm/src/router.rs` (tests), `crates/lattice-cli/src/commands/doctor.rs` (tests)
- **Issue:** Env-var tests previously used unsynchronized `std::env::set_var` / `remove_var`, which could race under parallel `cargo test`.
- **Fix:** Router tests now use a local env mutex helper; CLI doctor tests use shared env helpers for set/remove isolation.

### WR-03: SQLite per-method connection with no pooling — `consistency`
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** `self.conn()` opens fresh `rusqlite::Connection` per call. No pooling, no consistent snapshot between operations.
- **Fix needed:** Persistent `Connection` or `Mutex<Connection>` in struct, opened once at construction.

### WR-04: tool_output_is_ok fragile string prefix check — `correctness`
- **File:** `crates/lattice-agent/src/agent_loop.rs:504-506`
- **Issue:** Checks `!output.starts_with("Error:") && !output.starts_with("Tool denied:")`. Different error formats pass silently.
- **Fix needed:** `ToolCallExecutor::execute` returns `ExecutionResult { output, success }` or `Result<String, ToolError>`.

### WR-06: TOCTOU race in turn parent_commit_id resolution — `race-condition` [resolved 2026-05-19]
- **File:** `crates/lattice-api/src/turn/execute.rs`
- **Issue:** `head_transcript` was called again after agent loop for `parent_commit_id`. Concurrent turn could change HEAD between reads.
- **Fix:** Parent commit id is now captured from the initial transcript read and carried through turn execution into `TurnCommitInput`.

### IN-01: Duplicated test helpers across modules — `maintainability`
- **Files:** `crates/lattice-agent/src/agent_loop.rs`, `crates/lattice-api/src/turn.rs`, `crates/lattice-api/src/session.rs`
- **Issue:** `user_msg`, `assistant_msg`, `tool_msg` copied in 3+ test modules.
- **Fix needed:** Extract to `lattice-test-utils` crate or `tests/common`.

### IN-02: lattice-hooks crate unused — `dead-code`
- **File:** `crates/lattice-hooks/src/lib.rs`
- **Issue:** 405-line hook system never imported. Deferred per CLAUDE.md future work section.
- **Fix needed:** Remove crate and dependency, or gate with `#[cfg(feature = "hooks")]`.

### IN-03: lattice-cli depends on lattice-hooks but never imports — `unused-dep` [resolved 2026-05-19]
- **File:** `crates/lattice-cli/Cargo.toml`
- **Issue:** `lattice-hooks` was listed in CLI dependencies without any `use lattice_hooks::` in CLI source.
- **Fix:** Removed the unused `lattice-hooks` dependency from `lattice-cli`.

---

## PR #6 — 2026-05-19

5 unresolved P2 findings from code review.

### P2-01: Persist overridden model in turn commit metadata — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-api/src/turn.rs`
- **Issue:** `model_name` is recomputed from `loaded_profile` first, overriding `request.model` during persistence even though execution honors `request.model` in `resolve_model_and_prompt`. With a loaded profile plus `-m/--model` override, committed turn metadata records the wrong model, making `session log`/audit data inaccurate.

### P2-02: Treat existing NULL-tip branch as a valid checkout target — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** Lookup flattens an existing branch with `commit_id = NULL` into `None`, making it indistinguishable from a missing branch. Checking out an existing empty branch (e.g. `main` before first commit) falls through to commit-id resolution and returns `MissingCommit` instead of succeeding as a no-op branch checkout.

### P2-03: Use detached HEAD commit when branching without --from — `correctness` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** `from: None` path calls `current_branch_commit_id`, which returns `None` when HEAD is detached (only reads `head_ref`). `session branch <name>` fails with "no commits on current HEAD" immediately after `session checkout <commit-id>`, breaking the standard detached-to-branch workflow unless `--from` is manually passed.

### P2-04: Emit stream events incrementally during turn execution — `streaming`
- **File:** `crates/lattice-api/src/turn/execute.rs`
- **Issue:** `stream_turn_with_deps` awaits `execute_turn_inner` before constructing the `TurnEventStream`, so callers receive no events until the entire LLM/tool/commit pipeline finishes. In long generations or tool-heavy turns this defeats streaming semantics and can cause client-side timeouts or frozen UIs.

### P2-05: Keep session HEAD metadata in sync when moving a branch — `consistency` [resolved 2026-05-19]
- **File:** `crates/lattice-session/src/sqlite_graph.rs`
- **Issue:** Branch upsert updates `graph_refs.commit_id` but never updates `graph_sessions.head_commit_id` when the moved branch is currently checked out (`head_ref == name`). After `session branch <current-branch> --from <commit>`, status/list/reset metadata can report a stale HEAD commit.