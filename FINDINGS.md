# FINDINGS — Open Issues

未修复项，留给后续 alpha 主线。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

(None)

---

## MEDIUM

### M-24-01: `host_pattern` 字段名暗示通配符但实际只做精确匹配 — `api-shape`
- **位置**: `crates/lancet-native/src/effect/region.rs:270-272`、`crates/lancet-protocol/src/envelope.rs`（`FetchUrl { host_pattern }` 定义）
- **问题**: `AuthorizedEffectRegion::contains` 对 `ExternalObservation` 的 host 检查是 `h == host`（精确匹配），但 `FetchUrl` capability 字段名为 `host_pattern`，暗示 glob/通配符支持。若 capability 声明 `host_pattern: "*.example.com"`，实际 `api.example.com` 不会匹配。
- **当前状态**: 当前无通配符 host 的使用场景，行为正确但字段名误导。
- **计划**: 重命名 `host_pattern` → `host`（破坏性改动需协调 envelope 序列化），或实现简单通配符匹配（`*.example.com` → suffix match）。排期取决于是否有多 host 的 `web.fetch` envelope 需求。

### M-24-02: `capture_workspace_snapshot` 同步无界遍历阻塞 async runtime — `performance`
- **位置**: `crates/lancet-native/src/handlers/command.rs:365-442`
- **问题**: 对整个 project root 做同步 `std::fs::read_dir` + `std::fs::read` 递归遍历（仅排除 `.git`/`target`），无深度/文件数上限。每次 `WorkspaceMutation` 命令调用两次（pre + post snapshot）。大仓库会阻塞 tokio worker thread。
- **当前状态**: v1.3-alpha CLI 场景工作区规模可控，非 bug。
- **计划**: `tokio::task::spawn_blocking` 包装 + 加深度/文件数上限；或改用 mtime 比较 + 按需 hash。与 command handler async 签名无关（snapshot 函数独立）。

### M-24-03: `ToolLimits` 在 CEP policy 之后成为隐藏 native 限制 — `architecture` / `policy-consistency`
- **位置**: `crates/lancet-native/src/executor/limits.rs`、`crates/lancet-native/src/executor/shell.rs:214-256`、`crates/lancet-native/src/edit/apply.rs:14-248`
- **问题**: `ToolLimits` 仍以 legacy “tool limit” 形状存在，但 CEP 之后 read/network/command 限制已经分别由 `ObservationPolicyConfig`、`NetworkPolicyConfig`、`CommandPolicy` 管理。当前字段中 `max_read_size`、`max_http_response_size` 无读取点；`max_command_timeout` 只作为 `execute_argv(timeout_ms=None)` fallback，生产 command/verify 路径已传入 CEP policy 计算出的 timeout；只有 `max_write_size` 仍作为 `mutate.patch` 应用后的单文件写入硬上限生效。
- **当前状态**: 已从公共 `lattice-tooling` surface 收回到 `lancet-native` crate-private 实现细节，行为正确；但该结构会让后续读者误以为 native 层仍有一套独立 tool policy。
- **计划**: 后续统一重构时删除 `ToolLimits`。候选方案：将 patch 写入上限改成局部 `PatchApplyLimits { max_write_size }`；或提升为 CEP mutation policy（例如 `mutation.max_single_write_bytes`）并从 profile/external policy 下发。command fallback 应改为显式使用 `CommandPolicy::default_timeout_ms/max_timeout_ms`，避免 native helper 自带第二套 timeout 默认。

### M-24-04: `tools.enabled` 同时承担 LLM 暴露白名单和 CEP base authority 派生 — `architecture` / `policy-consistency`
- **位置**: `crates/lattice-api/src/turn/enabled_tools.rs`、`crates/lattice-api/src/turn/cep.rs:197-208`、`crates/lattice-api/src/turn/cep.rs:582-602`、`crates/lattice-api/src/turn/cep.rs:788-793`
- **问题**: `tools.enabled` 目前不只是 agent-visible method whitelist。它先过滤 catalog-derived LLM tool schemas，再生成 `CepExecutorAdapter` 的 enabled method set；同时又被 `production_base_envelope_from_policy` 用来派生 turn base envelope 能力：`session.envelope` / `session.todo_write` 触发 `MutateSessionState`，`fetch.web` 触发 allowed-host `FetchUrl` capabilities。这样 `tools.enabled` 同时承担 exposure 和部分 authority shaping，和 CEP policy/envelope 语义重叠。
- **当前状态**: 行为正确，且 adapter disabled-method 拒绝、base envelope session/audit/fetch capability 映射已有测试覆盖；但该路径容易让后续读者误以为 tool enablement 本身就是授权边界。实际最终 authority 仍应由 CEP policy、Guard、active envelope 和 native handler enforcement 决定。
- **计划**: 后续统一重构时拆开两层语义。`tools.enabled` 保留为 LLM exposure / adapter whitelist；base envelope 从 explicit CEP base-policy、catalog method requirements、profile `[lancet]` policy 或独立 authority builder 生成，不直接由 exposure list 推导。可将 `enabled_tools.rs` 重命名为 `tool_exposure.rs`，并把 base envelope 派生迁到 CEP authority composition 代码中。

### M-24-05: `lattice-api` 从薄 facade 膨胀为 turn runtime composition 层 — `architecture` / `crate-boundary`
- **位置**: `crates/lattice-api/src/lib.rs:1-11`、`crates/lattice-api/src/turn/cep.rs`、`crates/lattice-api/src/turn/live.rs`、`crates/lattice-api/Cargo.toml`
- **问题**: `lattice-api` 对外仍宣称是 thin public API facade，但内部已经承载 turn-scoped LANCET runtime composition：`CepExecutorAdapter`、catalog tool projection、profile → CEP policy lowering、command isolation lowering、external policy load/merge、base envelope 派生、attestation persistence、native registry/transport/audit/invariant 装配，以及 CEP event → public `TurnEvent` 映射。`turn/cep.rs` 已成为上千行的 dependency magnet，`Cargo.toml` 也直接依赖多枚 LANCET 内部 crate。
- **当前状态**: 对外 `run_turn` / `stream_turn` / `SessionApi` surface 仍然可用，行为正确；问题是 crate boundary 已经漂移，public facade 和内部 turn control plane 混在同一个 crate，后续 CLI/TUI/daemon 复用时容易把 API crate 变成事实上的 runtime 聚合层。
- **计划**: 后续统一重构时切出内部 turn/LANCET composition crate（命名需随边界再定，优先避免泛化成无边界的 `runtime` 桶）。`lattice-api` 保留 public request/response/event types、`SessionApi`、`run_turn` / `stream_turn` facade 和 session commit 入口；把 `CepExecutorAdapter`、`build_production_turn_deps`、profile/policy lowering、base envelope builder、native LANCET 装配、attestation/event mapping 迁到内部 crate。CLI/TUI/daemon 继续依赖 `lattice-api`，不直接穿透到 native internals。

## LOW

### L-20: `SqliteSessionGraphStore` 单连接 `Mutex` 应用层串行化 — `performance` / `architecture`
- **位置**: `crates/lattice-session/src/sqlite_graph/mod.rs`
- **问题**: 虽启用 WAL，但全进程共享一个 `Connection` + `Mutex`，读写均独占；多会话/多任务并行时 DB 成为串行瓶颈。
- **当前状态**: 本地 CLI 场景可接受；非 bug。
- **排期**: 除非很快做并发 TUI、多 session daemon 或 HTTP server，否则属 v0-beta 前架构评估项，非 v1.3-alpha 开场项。
- **计划**: 连接池或每任务独立 `Connection`（仍每连接 WAL）。

### L-21: 工具执行 `await_with_deadline` 未与 `cancel_flag` 竞争 — `robustness`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`（`execute` 调用处）
- **问题**: 用户取消后，须等当前工具跑完或触及 turn `deadline` 才能在 checkpoint #5 退出；长时 `bash` / `web_fetch` 取消滞后。
- **当前状态**: Plan 15-02 **D-07** 明确不 abort in-flight 工具。
- **升级条件**: 若 v1.3-alpha 引入 verification runner / long-running command workflow，**升为 Medium**。
- **计划**: `select!` cancel + 工具 future；对可中断工具传 cancel 或子进程 kill（按工具类型分级）。

### L-22: `ChatRequest` 每轮 clone 完整 provider message Vec — `performance` / `api-shape`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`
- **问题**: M-21 已消除每轮 GraphMessage→LLM Message 全量转换，但 `ChatRequest` 仍拥有 `Vec<Message>`，每轮 LLM 调用要 clone 当前完整 provider message buffer。
- **当前状态**: 本地 CLI 和短工具链路可接受；属于 API ownership 形状问题，非行为 bug。
- **计划**: 将 LLM 请求改为 borrowed/`Arc<[Message]>`/builder 复用一类设计，避免为性能小修破坏 provider adapter contract。

### L-23: `run_turn_inner` 551 行巨型函数 + 热路径密集 `clone` — `maintainability` / `performance`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`（`run_turn_inner`，约 127–678 行）
- **问题**: 单函数承载整个多轮 turn 状态机（前置组装 → 模型流消费 → 工具执行循环 → 提交），551 行、非测试区 ~48 处 `.clone()`。可读性/可测性差，且 clone 分散难以集中优化。与 **L-22**（每轮 clone `ChatRequest` message Vec）、已修 **M-21 residual** 同源——均为该函数 ownership 形状问题的不同切面。
- **当前状态**: 行为正确、测试覆盖充分（`agent_loop.rs` / `streaming_*.rs` / `live_sink_and_cancel.rs`）；属 altitude/可维护性异味，非 bug。零行为变更的重构，风险在于触碰核心热路径与 cancel/deadline/事件时序，需谨慎。
- **计划**（拆分方案，待按阶段实施）：
  1. **按 turn 阶段切子函数**，`run_turn_inner` 退化为编排骨架：
     - `assemble_turn_context`（读 session state、构建初始 messages/base envelope）
     - `run_model_stream`（已有 `consume_model_stream` 的上层，产出 assistant message + tool calls）
     - `execute_tool_round`（顺序执行工具 + checkpoint #5 cancel/deadline 检查 + 结果 append）
     - `commit_turn_outcome`（持久化 commit / 错误路径提交）
  2. **收敛 clone**：把每轮重建的 `GraphMessage` 改为 `std::mem::take` / 单次构造；与 L-22 合并处理 `ChatRequest` 的 `Arc<[Message]>` 化，使 message buffer 跨轮复用而非每轮 clone。
  3. **保持时序契约**：cancel checkpoint 编号（D-07）、live `try_send` best-effort 镜像（M-19a，见 `emit_agent_event` 注释）、`buffered` 为权威事件序列——重构后须有测试断言这三点不变。
  4. **分步落地**：先做纯函数抽取（结构不变、行为不变，过现有测试），再单独做 clone/ownership 收敛，避免一次大 diff 同时改结构与所有权。
- **排期**: 与 L-22 合并到一次 agent-loop 重构窗口；非 v1.3-alpha 开场项。

### L-29-01: Phase 29 tmpfs root 路径以 child PID 命名 — `isolation-supervisor` / `future-proofing`
- **位置**: `crates/lancet-native/src/executor/supervisor/syscall.rs:471-473`、`crates/lancet-native/src/executor/supervisor/syscall.rs:93-99`、`crates/lancet-native/src/executor/supervisor/syscall.rs:230-248`
- **问题**: `tmpfs_root_path()` 使用 `/tmp/lattice-supervisor-root-{pid}`。当前 Phase 29 在 child pre-exec 中创建并挂载 tmpfs root，PID 是正在 setup 的 child，且 root 只在 pivot 前短暂存在；并发命令不会共享同一个 child PID。风险只在未来如果把 tmpfs root 改为 parent 侧预建、跨进程缓存或提前复用路径时，PID 回收可能导致路径碰撞或清理误伤。
- **当前状态**: 当前实现安全，非 bug。`LiveSupervisorSyscalls::Drop` 在 pivot 前失败时会清理 `tmpfs_root`，pivot 成功后 `detach_old_root` 将 `tmpfs_root = None`，避免 parent-side Drop 误清理已经成为 `/` 的 tmpfs view。
- **计划**: 若未来把 mount root 创建移到 parent 侧或引入复用/并发预建，改用 `tempfile::TempDir`/`mkdtemp` 风格随机目录并把生命周期绑定到 setup attempt；保持 child pre-exec 现状则无需改动。

### L-29-02: Phase 29 setup report pipe 8192 字节读取上限 — `isolation-supervisor` / `diagnostics`
- **位置**: `crates/lancet-native/src/executor/shell.rs:680-699`
- **问题**: `SetupReportPipe::read_report()` 最多读取约 8192 字节后停止。当前 setup report 只包含紧凑状态标签、stage 名称和少量诊断，远低于该限制；如果后续把 mount 列表、完整 syscall diagnostics、Landlock rule 明细或长错误链塞进 setup report，可能截断 JSON，导致 parent 侧 `decode_setup_attempt_report` 失败并回退到 fallback metadata。
- **当前状态**: 当前 Phase 29 行为正确，非 bug。限制是隐含的诊断容量约束。
- **计划**: 若 setup report payload 扩展，增加显式长度前缀/完整读取上限错误、压缩 diagnostics，或把 verbose diagnostics 写入独立审计记录；同时加一个超过上限的回归测试，确认不会静默降级为误导性边界证据。

### L-29-03: FD cleanup 的 rlimit fallback 遇到 `close` 非 EBADF 错误会整体 fallback — `isolation-supervisor` / `robustness`
- **位置**: `crates/lancet-native/src/executor/supervisor/fd.rs:122-136`、`crates/lancet-native/src/executor/supervisor/fd.rs:222-232`
- **问题**: `cleanup_with_rlimit_loop()` 对每个 fd 调 `close_fd(fd)?`，`live_close_fd()` 只把 `EBADF` 视为成功；其他错误会中断 rlimit loop 并进入 `/proc/self/fd` fallback。Linux 上 `close(2)` 返回 `EINTR` 时 fd 状态语义微妙，fd 通常不能安全重试；当前把它当 error 是保守选择，但会把 cleanup method 从 rlimit loop 降级到 proc-fd loop。
- **当前状态**: 当前实现安全，非 bug。`close_range` 优先，rlimit loop 失败后还有 `/proc/self/fd` fallback；若最终 cleanup 失败，边界 evidence fail-closed，不会误给 fd cleanup credit。
- **计划**: 若需要减少 rare-platform 噪声，可把 `EINTR` 作为 “close attempted; continue without retry” 的特殊状态，或在 report 中记录 partial close/fallback reason；改动前需保留 non-stdio fd 不泄漏的 live canary。

### L-29-04: Phase 29 live canary 和 fake matrix 覆盖充分但用途不同 — `verification` / `release-gate`
- **位置**: `crates/lancet-native/src/executor/supervisor/linux.rs:809-874`、`crates/lancet-native/src/executor/supervisor/live_tests.rs:55-75`、`crates/lancet-native/src/executor/supervisor/env.rs:118-139`
- **问题**: Phase 29 的 coverage 由几类测试共同支撑：fake failure matrix 覆盖每个 supervisor stage 的 fail-closed 映射；attribution tests 确认 Landlock 不给 network credit、mount namespace alone 不给 filesystem credit；live canary 在真实内核上覆盖 netns/mount/fd/Landlock；env tests 注入 HOME、SSH agent、tokens、LD/Python/Node/Rust/Cargo 等危险 host vars 并证明 allowlist 构造不继承它们。这里没有单一测试同时证明所有维度，后续 release gate 不应把某一类测试的通过误读成完整 proof。
- **当前状态**: 当前覆盖与 Phase 29 目标匹配，非 bug。live canary 是 host-specific evidence；fake matrix 是 deterministic fail-closed proof；env tests 是 construction proof。
- **计划**: Phase 32 rollout gate 如需发布级证明，应保持三类证据分层展示：deterministic unit/fake、current-host live canary、profile/doctor host probe；禁止把 `skipped_unsupported` 或 fake-only evidence 当作 current-host physical proof。

### L-30-01: Overlay cleanup 只保留首个失败诊断 — `diagnostics` / `observability`
- **位置**: `crates/lancet-native/src/executor/supervisor/overlay.rs:393-425`
- **问题**: `cleanup_overlay` 使用首个失败作为 cleanup report 的失败原因；如果 unmount 失败后 upper/work 目录删除也失败，后续失败信息不会进入 compact metadata。安全性不受影响，因为任一 cleanup failure 都会阻止 artifact commit，但排查 busy mount、残留 upper/work、child cleanup 等复合故障时诊断信息会变薄。
- **当前状态**: 非阻断，Phase 30 安全关键路径正确；`cleanup_outcome=cleanup_failed` 足以让 artifact commit fail-closed。
- **计划**: 后续 Phase 31/32 或诊断增强窗口可把 cleanup report 扩展为保留首个主失败 + 附加失败列表，或引入 compact repeated failure records；保持现有 fail-closed 行为和 metadata 体积约束，补充多失败矩阵回归测试。

### L-31-01: artifact commit `passed` 路径的 `commit_baseline_details` 为空 — `commit-baseline-audit` / `observability`
- **位置**: `crates/lancet-native/src/executor/supervisor/artifact.rs:1050`、`crates/lancet-native/src/executor/supervisor/artifact.rs:1077`
- **问题**: `apply_prepared_artifact_set` 在 partial 和成功两条路径都写 `commit_baseline_validation = "passed"`，但 `commit_baseline_details = json!({})`，不带任何 hunk anchor summary。失败路径（`missing_anchor`/`hash_mismatch`/`ambiguous_hunk`/`lower_drift`/`verify_layer_contamination`）都会序列化 compact details，唯独成功路径没有验证证据，因此审计无法从 metadata 复现"哪些 hunk 通过了 baseline 校验"。
- **当前状态**: 非 bug。31-CONTEXT D-09 把 `commit_baseline_failure_reason` 和 details 标为 optional，passed 路径省略 details 是最保守、最省 metadata 体积的做法；`mutate.patch` 直写路径同样不在成功时记录 hunk。
- **计划**: 若后续需要 positive 审计证明（例如合规要求逐 hunk 可复现），在成功路径补 `commit_baseline_details.hunks` 紧凑摘要，复用 `commit_baseline_details_for_hunks`，仅记录 hash/offset/count 不带 anchor 源文本（保持 T-31-05 约束），并加成功路径 metadata 回归测试。

### L-31-02: `LowerBaseline.regular_file_contents` 全量缓存项目文件字节 — `performance` / `memory`
- **位置**: `crates/lancet-native/src/executor/supervisor/snapshot.rs:33`、`crates/lancet-native/src/executor/supervisor/snapshot.rs:251-263`
- **问题**: P31-WR-02 修复（commit `71e8caf`）让 `capture_lower_baseline` 在计算每个 regular file BLAKE3 hash 的同时把完整字节存入 `regular_file_contents`，使 commit-baseline anchor 校验读取 capture 时刻的快照而非活 workspace（消除 TOCTOU）。代价是整个 lower 视图的 regular file 内容常驻内存,大型 workspace 下峰值内存随项目总字节线性增长。
- **当前状态**: 非 bug，安全性正确。开销是线性而非指数，且只在 artifact overlay commit 路径短暂存在；v1.4-alpha 目标项目规模下不构成问题。`is_snapshot_skipped_path` 已排除 `target/`、`.git` 内部等大目录。
- **计划**: v1.5 大型 workspace 优化窗口可改为 lazy read-on-demand + 用已捕获的 content hash 做完整性校验（读时比对 hash，不匹配即判 lower drift），或只缓存带 hunk anchor evidence 的文件字节而非全树；改动需保留消除 TOCTOU 的不变量并补一个"capture 后、recheck 前修改源文件仍 fail-closed"的回归测试。

## P22.5 supervision verification (2026-05-28)

Live-scenario harness: `scripts/p22-real-scenarios.sh` (disposable workspaces under `build/tmp/p22-real-scenarios/`).

| Scenario | Result | Notes |
|----------|--------|-------|
| 01-bugfix | PASS | Rust fixture; `cargo test` verify; fix `src/lib.rs` only |
| 02-verify-repair | PASS | Requires `cep_verify_failed` then same-envelope repair (41→42); no auto-`ReplanRequired` on verify |
| 03-scope-replan | PASS | Requires `serde` in `Cargo.toml` + `cep_replan_required` or supersede path |
| 04-hard-denial | PASS | `GuardDenied` on `..` write; secret file unchanged |
| 05-docs-code | PASS | README + `src/greeting.rs` bounded diff |

**Harness fixes during bring-up:** fixture `Cargo.toml` needs `[workspace]` so `cargo test`/`check` under `build/tmp/` do not join the lattice repo workspace. Strict OpenAI-compatible gateways may reject dotted tool names — `lattice-llm` maps `a.b` ↔ `a__b` on the wire only (CEP catalog stays dotted). Some gateways (e.g. MiMo token-plan) accept dots without encoding. Scenario 04 judge checks the exact prompt path `../p22-escape-outside.txt` (no pre-check `rm`).

**Harness pass criteria:** scenario 02 fails if `cep_verify_failed` is absent; scenario 03 requires `cep_replan_required` or `cep_envelope_superseded` before `Cargo.toml` gains `serde`. Report artifact: `build/tmp/p22-real-scenarios/report.md`.

## P22.6 self-host supervision (2026-05-28)

Self-host harness: `scripts/p22-self-host.sh` (disposable clone under `build/tmp/p22-self-host/<run-id>/lattice-under-test`).

| Task | Default | Result | Notes |
|------|---------|--------|-------|
| task-a | yes | PASS | Doc-only `docs/p22-self-host-note.md`; verify `echo self-host-doc-ok`; no `crates/` diff |
| task-c | yes | PASS | Out-of-root canary read denied; `GuardDenied` on `..` escape; `ReplanRequired` on forbidden `crates/` write |
| task-b | opt-in (`P22_SELF_HOST_TASK=task-b`) | PASS (optional) | Fixture `tests/fixtures/p22-self-host/fixture-crate/`; verify `cargo test --manifest-path …` |

**Pass criteria:** outer checkout must not change; inner agent only mutates declared scope (or shows `ReplanRequired` / guard denial). Report: `build/tmp/p22-self-host/report.md`. Default verification is **task-a + task-c** from clean supervisor checkout with live `compat:deepseek-v4-pro` (2026-05-28). Task B is implemented but not part of the default merge gate.

---

## OBSOLETE / CLOSED

（完整修复记录见 [FIX.md](./FIX.md)。）

### H-03: `write_file` 跟随符号链接绕过 project-root 限制 — `security`
- **当前状态**: 2026-05-27 fixed。Unix 写入改为逐级 `openat` + `O_NOFOLLOW`，拒绝 symlink parent/leaf，关闭 `symlink_metadata`→`write` 的 TOCTOU 窗口。

### H-10: `validate_agents_dir` canonicalize 失败时回退原始路径 — `security`
- **当前状态**: 2026-05-27 fixed。不存在的 agents dir 先 canonicalize 最深存在前缀，解析其中 symlink，再拼接缺失后缀做危险路径判断。

### M-21: `AgentLoop` 每轮全量 `convert_to_llm_message` — `performance`
- **当前状态**: 消除每轮 GraphMessage→LLM Message 全量转换；tool round 仅 append。**残留**: 每轮 `ChatRequest` 仍 clone 全量 `Vec<Message>`，仍存在 `O(rounds × messages)`（见 FIX.md **M-21** Residual）。

### L-18-03: 无 stream 超时/deadline 机制 — `robustness`
- **原位置**: `crates/lattice-cli/src/commands/run.rs:439`
- **原问题**: `while let Some(event) = stream.next().await` 无超时，hung provider 会阻塞 CLI。
- **当前状态**: CLI 加 5 分钟 idle timeout，超时写 stderr + exit 1。API/Agent durable deadline 留未来。

## PARTIAL

### M-04: `validate_base_url` DNS 检查与 HTTP 请求之间存在 TOCTOU — `security`
- **已做**: 请求前 `resolve_public_socket_addrs`，`ClientBuilder::resolve_to_addrs` 钉住解析结果（`lattice-llm/src/client.rs`）。
- **残留**: `validate_base_url` 阶段的 DNS 仍为 best-effort（离线 CI 可能不解析）；权威防护在 request-time pinning，配置时校验不能单独当作闭环。

### M-05: `web_fetch` DNS rebinding / 黑名单 — `security`
- **已做**: 与 M-04 共享 request-time 钉 IP；已知 rebinding 后缀硬拒绝（`DNS_REBINDING_DOMAIN_SUFFIXES`，`security.rs` + `web.rs`）。
- **残留**: 黑名单不可穷尽（新 wildcard→IP 服务、非列表域名）；需持续维护或改为启发式（主机名编码 IP 模式）+ 仍依赖钉 IP 作为主防线。
