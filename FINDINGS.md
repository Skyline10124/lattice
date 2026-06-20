# FINDINGS — Open Issues

未修复项，留给后续 alpha 主线。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

(None)

---

## MEDIUM

### M-33-01: Profile `[workspace]` → `UpperSpec` 翻译未在 Phase 33 规划 — `planning` / `v1.5-alpha`
- **位置**: `.planning/phases/33-workspace-adapter-production-hardening/33-03-PLAN.md`（schema only）；composition 层无 plan task
- **问题**: Phase 33 交付 `WorkspaceSection` / `CredentialRef` 与 `UpperSpec` / `apply_egress`，但无 `lattice-api` 将 profile 解析结果映射为 adapter 调用的任务。PLAN-CHECK rev2 以 warning 放行，此前无正式 defer 台账。
- **当前状态**: 已记录于 `33-DEFERRALS.md` **D-33-01**；Phase 33 验收以 schema + adapter 单测/集成为准。
- **计划**: **Phase 34** turn/CEP 组合层实现 `WorkspaceSection` → `UpperSpec`（含 egress、credential 解析）；执行 Phase 33 时不得将此缺口判为 phase 失败。

### M-33-02: WS-04 按项目 warm template 自动选择未接线 — `planning` / `v1.5-alpha`
- **位置**: `crates/lattice-workspace/src/template.rs`（helpers）；`33-05-PLAN.md`（仅 `ImageOrTemplate::Template` 显式传入时 restore）
- **问题**: ROADMAP SC3 要求 per-project template 支撑 Upper 创建；Phase 33 有 SDK snapshot API 与 `WarmTemplate::is_stale`，但 `create_upper` Image 路径不会自动解析 `proj-<hash>-template`。
- **当前状态**: 已记录于 `33-DEFERRALS.md` **D-33-02**；V-05 覆盖显式 template 名路径。
- **计划**: **Phase 34** turn 启动编排，或 **33-05 执行期**扩展 05-T2；verify-phase 不得要求端到端自动 template 选择。

### M-33-03: 生产 adapter 上 WS-02 contract suite 非强制 — `verification` / `v1.5-alpha`
- **位置**: `33-05-PLAN.md` T3（`test_microsandbox_contract_suite` optional）
- **问题**: WS-02 路径泄漏契约在 `DeterministicWorkspace`（33-04）上保证；`MicrosandboxWorkspace` KVM lane 仅 V-05/V-08，contract suite 可选。
- **当前状态**: 已记录于 `33-DEFERRALS.md` **D-33-03**。
- **计划**: **33-05 执行时**将 integration lane contract suite 改为 mandatory（推荐）；非 Phase 33 planning blocker。

### M-33-04: Phase 33 计划/研究的 microsandbox SDK API 与锁定 crate 不符（已校正）— `planning` / `v1.5-alpha`
- **位置**: `33-RESEARCH.md`、`33-PATTERNS.md`、`33-01/02/05-PLAN.md`、`33-VALIDATION.md`
- **问题**: RESEARCH/PATTERNS 依 Context7 文档推导 SDK 面，三处 API 在 `microsandbox 0.5.6` 实际 crate 中**不存在**：`Snapshot::save(name,&sandbox)`、`Snapshot::get(name).restore_with(...)`、`NetworkPolicy::allowlist([...])`；`apply_egress` 签名按值收发 `microsandbox::SandboxBuilder`（crate root 未导出，实为 `microsandbox::sandbox::SandboxBuilder`）。`33-PLAN-CHECK` rev3 PASS 仅做结构/依赖/覆盖校验，未对 SDK 调用核对源码。按原计划执行 33-05 会编译失败。
- **当前状态**: 已校正（2026-06-13）。六份文件改为对照 `~/.cargo/.../microsandbox-0.5.6/lib/` 源码验证的真实 API：save=`Snapshot::builder(src).destination(Name(t)).create()`、restore=`Sandbox::builder(new).from_snapshot(t).create()`、allowlist=`NetworkPolicy::none().allow_domains([...])?`、类型路径 `microsandbox::sandbox::SandboxBuilder`；`apply_egress` 拆出可测纯函数 `plan_egress→NetPlan`（让 V-06/V-07 det 层从空转变真断言）；RESEARCH 顶部加 API-CORRECTION 横幅；`33-PLAN-CHECK` 加 rev4 注记。
- **残留**: Wave 0 新增 **A4**（真机快照往返探针）与修正后的 A7 尚未在 KVM 宿主执行；若 A4 实测的 save/restore 调用形状仍与上述不符，33-05 / 33-PATTERNS Pattern 4 须按 A4 记录的逐字调用再校。plan-checker 未重跑（仍 rev3 结构结论 + rev4 注记）。
- **计划**: 执行 33-01 时跑 A4/A7 实证；A4 PASS 即闭环，FAIL 则按记录修 33-05 后闭环。根因教训：LLM/SDK 集成的 API 形状以锁定 crate 源码为准，Context7/训练记忆仅作线索。

## LOW

### L-34-01: SIGKILL/崩溃在 overlay teardown 之前会遗留孤儿 scratch，无启动清扫 — `robustness` / `hygiene`
- **位置**: `crates/lancet-native/src/executor/supervisor/overlay.rs`（`cleanup_overlay`）；scratch 根由 `OverlayPlan::deterministic` 建于 `<project_root>/../.lattice-overlays/<mode>-<sess>/`。
- **背景**: 正常 teardown 路径已正确清理 —— `remove_overlay_tree`（`syscall.rs`）在 `remove_dir_all` 前递归 `chmod 0o700`，能穿过内核建的 mode-000 `work/work`（commit `84847387`, 2026-06-07）。
- **问题**: 仅当进程在 `cleanup_overlay` **之前**被 SIGKILL / panic / 断电时，session scratch 不会被回收，留下 mode-000 `work/work` 孤儿。无启动期清扫来回收上次崩溃的 `.lattice-overlays`。
- **影响**: 低。孤儿曾经会让全树遍历 `EACCES`（`capture_workspace_snapshot` 在受控命令预检时硬失败，"连 `ls` 都过不去"）。该症状已由 `is_snapshot_skipped_path` 跳过 `.lattice-overlays` 缓解（2026-06-11，含复现 000 陷阱的回归测试）；剩余仅为磁盘累积与对"消费方记得跳过"的依赖。
- **计划**: executor 启动时清扫 `.lattice-overlays` 下的陈旧 session（按 mtime / pid 存活判定）；次要：`is_snapshot_skipped_path` 的内部 scratch 名（`target` / `.git` / `.lattice-overlays`）为散落字面量，可收拢为集中常量集。

### L-34-02: Direct-mode `changed_paths()` porcelain enforcement weaker than Envelope snapshot diff — `security` / `defense-in-depth`
- **位置**: `crates/lattice-api/src/turn/workspace_backend.rs:380-391`（`changed_paths` via `git status --porcelain=v1`）；`crates/lancet-native/src/handlers/command.rs:2438-2470`（post-exec scope check）。
- **问题**: Direct-mode post-execution write-scope enforcement uses `git status --porcelain=v1`, which omits all gitignored paths. Envelope-mode uses filesystem snapshot diff (`capture_workspace_snapshot` / `compute_changed_paths`) that only skips `target/`, `.lattice-overlays/`, `.git/`. A Direct workspace-mutating command can write to gitignored paths outside `allowed_write_paths` (e.g. `.env`, `build/`) and pass the gate because porcelain omits ignored paths. Additionally, a TOCTOU exists: a background process spawned by the command can write after `exec` returns but before `changed_paths()` runs.
- **当前状态**: ACCEPTED RESIDUAL. The Upper workspace is per-turn (created in `build_production_turn_deps`, destroyed when `ProductionTurnDeps` dropped). Session graph, audit trail, and credential store all live in the Lower (host) and cannot be affected by Upper writes. The LLM already has `RunCommand` capability, so writing to gitignored paths within a turn doesn't expand its attack surface. The porcelain gap weakens the `allowed_write_paths` defense-in-depth policy *within* the Upper, but creates no host-impact path.
- **升级条件**: If the Upper ever becomes per-session (gitignored writes persist across turns) → escalate to REQUIRES FIX. If Upper→Lower write-back is added (e.g., artifact commit) → escalate. If Upper gains network egress → TOCTOU becomes exploitable for exfiltration.
- **计划**: Optional future work: switch `changed_paths()` to filesystem snapshot diff for parity with Envelope mode. Do NOT use `--porcelain --ignored` (false positives on `target/` build artifacts).

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

### L-31-02: `LowerBaseline.regular_file_contents` 全量缓存项目文件字节 — `performance` / `memory`
- **位置**: `crates/lancet-native/src/executor/supervisor/snapshot.rs:33`、`crates/lancet-native/src/executor/supervisor/snapshot.rs:251-263`
- **问题**: P31-WR-02 修复（commit `71e8caf`）让 `capture_lower_baseline` 在计算每个 regular file BLAKE3 hash 的同时把完整字节存入 `regular_file_contents`，使 commit-baseline anchor 校验读取 capture 时刻的快照而非活 workspace（消除 TOCTOU）。代价是整个 lower 视图的 regular file 内容常驻内存,大型 workspace 下峰值内存随项目总字节线性增长。
- **当前状态**: 非 bug，安全性正确。开销是线性而非指数，且只在 artifact overlay commit 路径短暂存在；v1.4-alpha 目标项目规模下不构成问题。`is_snapshot_skipped_path` 已排除 `target/`、`.git` 内部等大目录。
- **计划**: v1.5 大型 workspace 优化窗口可改为 lazy read-on-demand + 用已捕获的 content hash 做完整性校验（读时比对 hash，不匹配即判 lower drift），或只缓存带 hunk anchor evidence 的文件字节而非全树；改动需保留消除 TOCTOU 的不变量并补一个"capture 后、recheck 前修改源文件仍 fail-closed"的回归测试。

### L-llm-coupling: LANCET 栈仍依赖 `lattice-llm` 叶子 crate — `architecture` / `crate-boundary`
- **位置**: `crates/lancet-native/src/executor/web.rs` 与 `crates/lancet-authority/src/policy/network.rs`(`lattice_llm::security`)、`crates/lancet-api`(经 native/authority 传递)
- **问题**: M-24-04 把 CEP-facing `ToolDefinition`、`ToolExposure`、`BaseAuthorityPlan` 抽到 `lancet-cep`，catalog→tool 投影不再依赖 `lattice_llm::ToolDefinition`；但 LANCET 栈仍未 100% 脱离 LATTICE，因为 URL/DNS 守卫复用 `lattice_llm::security`。
- **当前状态**: 非 bug。`lattice-llm` 是 Layer 0 叶子，不构成依赖环；LANCET 已摆脱 provider tool spec 耦合，但仍依赖一枚 LATTICE 网络安全 helper。
- **计划**: 选定 URL/DNS security 共享类型的归属后再做：把 `security` 的 URL/DNS 守卫抽到 LANCET 自有或中立共享 crate（如 `net-guard`），让 `lattice-llm` 与 LANCET 同时依赖它，从而让 `lancet-api` 彻底无 `lattice-*` 传递依赖。

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

### M-33-05: 生产 Upper 每次 create 在 VM 内 apt-get 装 git — 慢且与 Blocked egress 不兼容 — `production-hardening` / `performance`
- **当前状态**: 2026-06-14 fixed by Plan 33-06（gap-closure，Wave 4）。新增 `MicrosandboxWorkspace::build_git_ready_template`：一次性把 git 烤进按项目 warm template 快照（create → 固定字面量 `APT_GET_GIT` 经 `exec_with` argv 装 git → 校验 `git --version` → `stop()` → `Snapshot::builder(name).destination(Name(name)).create()`），T-33-04 不注入任何凭据、T-33-05 不用 `sandbox.shell()`。`deliver_baseline` 加固：`ImageOrTemplate::Template` 缺 git 直接报错（不再静默产出无 git 的 Upper）；`Image`+Open/Allowlist 仍一次性 apt-get 装 git（装不上则报错）；`Image`+Blocked 仅交付文件（exec-only Upper）；删除旧的"吞掉 apt-get 失败再静默跳过 git-init"路径。契约套件重构出 `run_suite_with` + `minimal_upper_spec_with`，`test_microsandbox_contract_suite` 改为从 git-ready template 在 `EgressPolicy::Blocked` 下跑，新增 `test_git_ready_template_has_git_under_blocked_egress`。验证：`cargo test -p lattice-workspace` 确定性 lane（无 KVM，23 测试）全绿，`cargo clippy -p lattice-workspace --features=integration_tests --all-targets -- -D warnings` 通过。**KVM 宿主实跑（2026-06-14，libkrun）**：`test_microsandbox_contract_suite` PASS（222s，此前 >595s 超时——单次建模板 + 6 次复用，零 per-Upper apt-get）、`test_git_ready_template_has_git_under_blocked_egress` PASS（188s，Blocked egress 下 `git rev-parse --show-toplevel` exit 0，git 来自模板而非 apt-get）。验证期发现并修复一处测试接线 bug（`6e96505`）：独立测试原误用 `restore_upper`（原始 CoW 恢复，不交付 baseline → 无 /workspace，导致 `exec_with(...).cwd("/workspace")` spawn ENOENT），已改为 git-ready 基底模板的正确每轮路径 `create_upper(ImageOrTemplate::Template)`（from_snapshot + deliver_baseline，与契约套件一致）。完整记录见 FIX.md M-33-05（2026-06-14）。
- **残留（非本 finding）**: D-33-02 自动 per-project template 选择（`create_upper` 的 `Image` 路径自动解析 `proj-<hash>-git-ready-template` / 查 `WarmTemplate::is_stale`）仍未接线，留 Phase 34（见 M-33-02）。WS-04 显式 git-ready-template 路径已满足。

### M-24-01: `FetchUrl` host 字段名与精确匹配语义收敛 — `api-shape`
- **当前状态**: 2026-06-05 fixed/stale-record cleanup。当前协议源码已是 `Capability::FetchUrl { host }`，root finding 和 crate-local `AGENTS.md` 中的 `host_pattern` 记录已移除。

### M-24-04: `tools.enabled` 同时承担 LLM 暴露白名单和 CEP base authority 派生 — `architecture` / `policy-consistency`
- **当前状态**: 2026-06-06 fixed。新增 `lancet-cep` facade；`MethodDescriptor::base_authority` 在 `lancet-catalog` 声明 method-local base requirements，`ToolExposure` 负责 catalog method exposure 并聚合这些 requirements，`BaseAuthorityPlan` 显式生成 session/fetch/audit base envelope 且继续拒绝 workspace write/command authority。`lancet-api` 不再用 hard-coded method-name 分支派生 base grants；`lancet-native` 不再携带 projection shim；`lattice-api` 只在 LATTICE↔LANCET seam 将 CEP tool definitions 转成 provider tool definitions。

### M-24-02: workspace snapshot 阻塞 async runtime — `performance`
- **当前状态**: 2026-06-05 fixed。command handler 的 workspace snapshot、lower baseline/fingerprint、artifact commit lower recheck/real commit 进入 blocking pool；snapshot helper 增加 entry/depth 上限。

### M-24-03: `ToolLimits` 隐藏 native 限制 — `architecture` / `policy-consistency`
- **当前状态**: 2026-06-05 fixed。删除 `ToolLimits`，shell executor 改为必传已校验 timeout，patch 写入上限迁入 `PatchApplyLimits`。

### L-30-01: overlay cleanup 只保留首个失败诊断 — `diagnostics`
- **当前状态**: 2026-06-05 fixed。cleanup report/metadata 保留首个失败摘要，同时新增 compact `cleanup_failures` 列表。

### L-31-01: artifact commit `passed` 缺少 hunk baseline details — `observability`
- **当前状态**: 2026-06-05 fixed。有 hunk evidence 的 passed/partial commit report 记录 compact `commit_baseline_details.hunks`；无 evidence 时继续保持 `{}`。

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
