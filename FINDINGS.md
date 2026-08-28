# FINDINGS — Open Issues

未修复项，留给后续 alpha 主线。已修复项见 [FIX.md](./FIX.md)。

> Issue-number migration (2026-07-24): historical bare issue references in this ledger use Codeberg numbering. Translate Codeberg #1 to GitHub #23 and Codeberg #2–#50 to GitHub #24–#72 (`github = codeberg + 22`); new references use GitHub numbers.
---

## HIGH

### H-GH34-03: reserved binding identity is not checked against the current compiled descriptor — OPEN, required before #35 — `integrity` / `lancet-api` / `lancet-runtime`

- **Location**: `crates/lancet-api/src/runtime.rs`（dispatch）；
  `crates/lancet-protocol/src/binding.rs`（`matches_request`）；
  `crates/lancet-runtime/src/execution.rs`（method lookup）。
- **Problem**: `admit_reserved` 会精确校验调用 binding 与 durable reservation 一致，
  但 dispatch 的 request check 只比较 method/arguments digest；执行阶段再按 method 选择
  current compiled Tool。reservation 中的 `descriptor_identity` 因此没有与当前 catalog
  descriptor 做 exact equality gate。
- **Current status**: OPEN，PR #178 非阻塞 residual；当前 production binding 与 runtime
  都从同一 turn-scoped catalog 生成，尚无 active-version 切换。operator Bundle activation
  会让该假设失效，因此本条是 #35 的前置条件。comment `5454533817` 已复核确认。
- **Plan**: 在 admission 前从 current enabled catalog 取 method 的 compiled identity，与
  binding identity 精确比较并 fail closed；同时让 model-visible 与 executable enabled set
  共享同一选择结果。不要增加第二个 catalog authority。

### L-GH139A-12: `session_journal_pin` grows with closures already in the Turn — OPEN, non-blocking follow-up — `performance` / `lattice-session` / GitHub #153

- **Location**: `crates/lattice-session/src/git_graph/pending.rs`
  (`StepClosing` candidate pin);
  `crates/lancet-audit/tests/invocation_qualification.rs`
  (`session_journal_pin_ns`).
- **Problem**: focused series show pin cost increasing across bindings inside a
  step and resetting at the step boundary. The current harness bounds the Turn,
  but the implementation still repeats closure-list work.
- **Current status**: OPEN under #153, not a #139-A/#139-B blocker. A focused
  syscall profile also found durability waits, rather than repository flock
  acquisition, dominating stalled runs; this must be separated from the
  deterministic closure-list growth before choosing a repair.
- **Plan**: add a local table-driven growth regression, remove repeated
  validation/traversal without changing durable order, and redesign performance
  evidence around in-span syscall and host-I/O attribution.

### L-GH139A-10: InvocationStore durability qualification needs a stable measurement model — DEFERRED — `qualification` / `lancet-audit` / GitHub #153

- **Location**: `benchmarks/invocation-store/`;
  `docs/plans/2026-08-21-issue139a-qualification-contract-v2.md`.
- **Problem**: retained campaigns produced valid but host-stall-sensitive verdicts
  that did not provide a stable decision boundary for the production hard cut.
  The latest recorded FAIL remains evidence; it is not reclassified as a pass.
- **Current status**: the qualification is diagnostic-only. #139-A is approved
  on correctness and structural evidence, and #139-B may proceed. The official
  runner uses `benchmark`; strict `qualify` remains for follow-up development.
  Benchmark bundles are local ignored artifacts below
  `target/invocation-store-bundles` and are not tracked in Git.
- **Plan**: #153 owns the replacement contract, including fsync amplification,
  host-I/O attribution, representative workload shape, and a repeatability
  requirement before any performance result becomes blocking again.

---

## MEDIUM

### M-TOOL-01: ADR 0004 build-closed WASM Component Tool runtime — hard cut implemented; identity/Bundle/docs remain — `architecture` / `lancet-runtime`
- **Location**: `docs/adr/0004-build-closed-wasm-tool-catalog.md`; `crates/lancet-runtime`; Source Tools under `tools/`.
- **Problem**: PR #178 implements runtime convergence; the pre-#35 descriptor-identity residual remains, while operator Tool Bundle installation/activation and the final architecture-document closeout are separate work.
- **Current status**: PARTIAL — the five default Tools are manifest-owned Components in one generated closed runtime. The two blocking PR-review defects (admission ambiguity and cancellation handoff) are fixed; `H-GH34-03` must close before #35, and #35/#36 remain.
- **Plan**: close `H-GH34-03`, then execute #35 and #36 without adding a second Tool schema, runtime plugin loader, local third-party Cargo build, or ambient WASI authority.


### ENV-OOM-01: 开发机常驻 Electron app 重度超售撞 24 并发 cargo 编译 → global OOM(已缓解,非代码 bug)— `environment` / dev-loop / #27
- **位置**: `xtask/src/ci.rs`(test step,已修);开发机环境(非 repo 代码)。完整根因账本在 `/tmp/lattice-oom-investigation/`(/tmp,不进 repo)。
- **问题**: `cargo test --workspace`(原 `xtask/src/ci.rs:44`)在 owner 的 24 核开发机上两次触发 **编译阶段** global OOM 致强制重启(2026-07-07 00:41:53 / 00:57:17)。00:41 OOM dump(348 个 uid 1000 进程)关键数:总 RSS 5.91 GB(物理 31 GB,未爆)、shmem 8.60 GB、swapents 8.42 GB(swap 14 GB 接近见底)、**total_vm 7478 GB(极度超售)**。编译工具链 RSS 合计仅 **2.41 GB**(rustc×24 + rust-lld×19 + cargo×1 + rust-analyzer×2),不是元凶。元凶是常驻 Electron app(electron×31 / obsidian×9 / cursor / codex / QQ / 微信 / wps / msedge ...)的 V8 巨型地址预留(total_vm 7478 GB)+ swap 见底。cargo 默认起 24 个 rustc(=核数),瞬时 codegen 分配是压垮骆驼的最后一根稻草。OOM killer 按 oom_score 杀 **adj=300 的 Electron**(不是 adj=200 的 rustc)→ 桌面崩溃 → 用户强制重启。`oom-kill:constraint=CONSTRAINT_NONE,global_oom`。**Codeberg #27 原文怀疑"某测试 spawn 大量子进程/proptest 爆炸"——日志否定:崩溃时无任何 test binary,纯编译阶段。**
- **当前状态**: MITIGATED（via `xtask ci`，见 FIX.md `#27`）。测试按 crate 串行运行，每步由 `LATTICE_TEST_TIMEOUT_SECS`（默认 300 s）限制，并用 `process_group(0)` + `kill -KILL -<pgid>` 回收整棵进程树；`CARGO_BUILD_JOBS`（默认 4）限制 fmt/clippy/build/test 的编译并发。#32 删除了会触发 DNS 黑洞的 native `fetch_effect_contract_negative`，相应的跳过开关也已删除。缓解不依赖关闭应用，但运行门禁前关闭非必要 Electron 应用仍可降低主机超售风险。
- **升级条件**: 若 jobs=4 cap 在常驻 app 全开时仍 OOM(更重型编译单元、更大 codegen)→ 升为 HIGH,考虑降 jobs 到 2 或加 zram。
- **计划**: (a) 已落地 xtask ci 改造;(b) 推荐 owner 跑门前关非必要 Electron app(非强制);(c) Linux CI runner 资源充足且没有桌面 app,不受影响,无需改 CI 配置。**#27 issue 应补 comment 修正方向**:真凶是环境侧超售,非 lattice 测试 bug,指向本 finding 与 FIX.md `#27` 条目。




### M-GH100-01: ~~same-parent Turns shared one derived `turn_id`~~ — CLOSED by #139-B
- **Resolution:** `TurnCoordinator::IdSource` now allocates one canonical UUID `TurnId` per accepted Turn; the typed ID is stored in PendingTurn, every binding, the v4 Turn manifest, permanent `turns/<TurnId>` pin, operator records, and public events. Canonical identity includes that explicit TurnId, so otherwise-identical siblings remain distinct. Regression coverage includes `random_id_source_emits_strictly_parsable_ids`, `explicit_turn_id_distinguishes_identical_siblings`, and the Commit 7 same-parent operator isolation test.

## LOW

> 下列 `L-39-*` / `ENV-WASM-01` / `T-39-01` 中的 `#39` 一律指 **GitHub #39**(= Codeberg #17,
> operator audit 下沉 + PR #74 Local Workspace 缺陷修复)。仓内其他文档里裸写的
> `#37/#38/#39/#40` workstream A 编号是 Codeberg 编号(GitHub #59–#62),是另一张票。

### T-GH143-01: `sigkill_mid_create_leaves_only_absent_or_fully_valid_stores` 的 120ms 窗口隐含假设 TMPDIR 在 tmpfs 上 — `verification` / test-hygiene / `lattice-store`
- **位置**: `crates/lattice-store/tests/integration.rs`（父进程 `sleep(120ms)` 后 SIGKILL 子进程，随后断言 `published > 0`）。
- **问题**: 子进程要先完成进程启动，才开始建仓循环；而建仓是 fsync 密集的。`tempfile::tempdir()` 用 `TMPDIR`，在 tmpfs 上这些 fsync 近乎免费，在真实块设备上则慢一个量级。把 `TMPDIR` 指向 nvme 后子进程在 120ms 内一个仓都发布不出来，断言以 `child should have published some stores` 失败——看上去像持久性缺陷，实际是计时假设。测的不变量（"每个目标要么不存在，要么是完整有效的仓"）本身没问题，脆的是"窗口内一定发布过东西"这个前提。
- **当前状态**: OPEN，不影响 CI。GitHub CI runner 的 `/tmp` 是 tmpfs，该测试在那里稳定通过（本地同样条件下 0.16s 全绿）。2026-08-22 在本机为绕开 `ENV-DISK-01` 的 tmpfs 配额而改用磁盘 `TMPDIR` 时首次撞到。
- **计划**: 把"发布过至少一个仓"改成由子进程自己汇报的事实（例如子进程每建一个就写一行到管道，父进程读到第一行后再 SIGKILL），而不是靠固定 sleep 猜。或者在断言前先确认子进程确实进入了循环，`published == 0` 时跳过而非失败。不要单纯调大 sleep——那只是把同一个假设推远一点。

### L-GH98-03: `net-guard` 在 async 调用栈上同步 `to_socket_addrs`（阻塞 `getaddrinfo`）— `robustness` / `net-guard` / GitHub #98
- **位置**: `crates/net-guard/src/lib.rs:140`（`resolve_public_socket_addrs`）、`:229`（rebinding 检查）；由 `lattice-llm` 的 `build_http_client_for_url` 内联调用，位于 LLM/fetch 的 async 栈上。
- **问题**: `to_socket_addrs()` 是阻塞的 `getaddrinfo`，直接跑在 reactor 线程上。DNS 慢或超时时会占住一个 tokio worker；单线程 runtime 上会停掉整个 turn。
- **当前状态**: OPEN。GitHub #98 的第 1、2 项（`SymlinkRejected` 回吐宿主绝对路径、`WorkspacePath::parse` 的 `..` 子串拒绝）已于 2026-08-22 修复并记入 `FIX.md`；第 3 项未动，本票因此保持 open。
- **计划**: 把解析放进 `spawn_blocking`，或换异步 DNS。注意 `net-guard` 目前只依赖 `url` 与 `std`（根 `AGENTS.md` 的 crate 表把这一点写成契约），所以引入 tokio 会改变该 leaf 的依赖形状——要么由调用方负责 `spawn_blocking`（推荐，保住 leaf 的中立性），要么先记一条 ADR 说明为什么 leaf 可以依赖运行时。

### L-GH144-01: ~~clean UUID-collision revision is unmodellable with synthetic Invocation facts~~ — CLOSED by #139-A
- **Location**: `crates/lattice-session/src/pending/facts.rs`, `crates/lattice-session/src/pending/reconcile.rs`, `crates/lancet-audit/src/invocation_store.rs`.
- **Problem**: (original) `InvocationFactsSnapshot.invocations` was a flat `InvocationId -> InvocationFact` map with no plan ownership, so clean collisions were indistinguishable from corruption.
- **Resolution**: #139-A adds `InvocationRecordFact { owner: PlanKey, lifecycle }` with mandatory owner and exact candidate key-set validation, `PlanValidation::Collision`, and a typed `ReconciliationDecision` whose `ReviseCandidates(CandidateRevisionRequest)` variant carries no `TurnOutcome`, plus a non-production `InvocationStore` on the `lattice-store` kernel and `GitSessionGraphStore::execute_candidate_revision`. The boundary test `a_different_durable_reservation_stays_fail_closed` is replaced by truth-table tests (`collision_at_binding_planned_requests_candidate_revision`, `collision_at_bound_is_corruption_not_a_revision`, `different_durable_reservation_is_corruption`, `exact_durable_reservation_does_not_re_reserve`) **and** by a real store-backed end-to-end test, `lancet-audit`'s `clean_collision_revises_candidates_end_to_end_over_real_stores`: foreign owner in a real `InvocationStore` → pinned facts → projection → pure reconcile → fresh UUIDs → real SessionStore CAS → reopen → complete revised plan → re-projection/reconcile, including CAS mismatch and post-CAS durability retry. Decision record: `docs/plans/2026-08-17-issue139a-decision-record.md`.

### L-GH144-02: ~~legacy outcome projection remained shipped~~ — CLOSED by #139-B
- **Resolution:** the v4 Turn manifest stores only typed `TurnOutcome`, `ClosureOrigin`, explicit `TurnId`, aggregate usage, and `WorkspaceHistoryCheckpoint`. `WorkspaceDisposition`, lifecycle evidence, `terminal_reason`, `legacy_status()`, and `turn/audit.json` are deleted; structural and serde regressions reject their return.

### L-GH139A-03: ~~the RESULT_FINALIZE_STRESS qualification and the FinalizeBudget it fixed are withdrawn~~ — CLOSED by the 2026-08-18 requalification
- **Location**: `crates/lancet-audit/tests/invocation_qualification.rs` (`result_finalize_stress`); `docs/plans/2026-08-18-issue139a-qualification-approval.md`; ADR 0005 §12; `target/invocation-store-bundles/20260817T214110Z-7c64a4f3ec34/result-finalize.jsonl` (local ignored historical artifact).
- **Problem**: the harness that produced the accepted result-finalize distribution published Invocation terminals while the PendingTurn journal was still `Bound`, and asserted `Bound` at the end of every step. ADR 0005 §12 requires the exact `ResultDraft` candidate to be pinned in a durable `StepClosing` snapshot before the draft write, and the pure reducer classifies a terminal fact in the binding window as corruption. The recorded finalize latencies therefore never measured the real durable order, and the 60-second `FinalizeBudget` derived in part from that group's 2.042 s maximum is not supported by evidence.
- **Resolution**: CLOSED for the Bound-vs-StepClosing harness defect only. The corrected harness was requalified on 2026-08-18 in bundle `20260818T115511Z-4a51b5d42484` (20 local-ledger + 20 result-finalize Arch Linux/Btrfs release runs, environment schema v2). That campaign remains historical evidence; the 2026-08-22 maintainer disposition separately approves #139-A and unblocks #139-B while moving performance qualification to #153. The withdrawn result-finalize group and its numbers remain retracted; its byte-identical bundle is retained locally below `target/invocation-store-bundles`.
- **Original status**: WITHDRAWN, blocking. The harness has been rewritten to drive `BindingPlanned -> exact reservation -> Bound -> StepClosing(cursor 0, no candidate)` and then, per binding, `DispatchStarted -> StepClosing(cursor i + exact candidate) -> draft path -> terminal + evidence -> StepClosing(cursor i+1, closure appended)`, ending at `StepClosed(ContinueModel)` and to prove by reopen/reduce that the durable state closes legally; it also asserts the two repository write locks are never held together. It has been run **once** as a functional check. The raw bundles were not rewritten and are retained byte-for-byte in the local ignored artifact directory; the current Git tree no longer tracks them. The local-ledger group remains valid historical evidence.
- **Original plan (executed)**: a maintainer authorized the fresh 20+20 run, the new immutable bundle was retained, and this approval commit re-derives the `FinalizeBudget` and the result-finalize regression thresholds from it.



### L-64-01: two accepted Rewind residuals — unrecorded mutations and cross-session range — `robustness` / `lancet-runtime` / `lattice-api` / #64
- **Location**: `crates/lancet-runtime/src/{history,host_mutate}.rs`; `crates/lancet-api/src/rewind.rs`; `crates/lattice-api/src/rewind.rs`.
- **问题**: (a) 安装成功但 history 记录失败的 mutation(#63 设计上只留 `workspace_history.recorded=false` 审计证据 + `tracing::error!`)对 Rewind 的回溯遍历不可见,因此不会被反转;(b) private history 按 project 而非 session 分片,Rewind 会反转目标 Turn 之后的**全部** in-range mutation,包括另一个 session 安装的。
- **当前状态**: ACCEPTED,两者都有测试覆盖。(a) 的防线是 preflight:未被反转的路径若已偏离记录的 post-state 会报 conflict 而不是被覆盖,不会静默丢用户内容;真正丢失的只是"这条 mutation 无法被 Rewind 撤销"。(b) 是唯一与 workspace 一致的选择——共享 Local Workspace 只有一个真实状态;审计记录的每个 step 都带 `turn_id`,可追溯是哪个 Turn 装的(`a_rewind_reverses_in_range_mutations_from_every_session`)。
- **升级条件**: 若 (a) 的记录失败率在真实使用中不可忽略(而不是磁盘故障级别的罕见事件),或用户报告 (b) 造成意外的跨 session 回滚 → 升为 MEDIUM,分别考虑"记录失败即让 mutation 报错"与"跨 session in-range step 需显式确认"。
- **计划**: 暂不改。任何改动都不得引入第二个 history store、补偿提交或第二个用户可见的 rewind 入口。

### L-63-01: two pre-existing test-isolation races surface under `cargo test --workspace` — `robustness` / test-hygiene / `lattice-session` / `lattice-api`
- **Location**: `crates/lattice-session/src/test_utils.rs:20-50` (`init_empty_repo`) with the fixed fixture root `/tmp/lattice-session-git-graph-tests/<name>`; `crates/lattice-api/tests/consistency.rs:162` and the sibling `usage_turn.rs` / `release_e2e.rs` fixtures (`std::env::set_current_dir`).
- **Problem**: Two independent races, both observed while verifying GitHub #63, neither caused by it (both files are untouched by that change).
  (a) `lattice-session`'s git-graph fixtures init repositories under a **fixed** `/tmp` path instead of a temp dir. A leftover directory from an earlier run makes `gix::init` fail with `Init(DirectoryExists)`; 16 tests fail at once. `init_empty_repo` already recovers from `DirectoryExists`, but only inside the `CurrentDir` error branch, so a plain leftover directory still panics.
  (b) `lattice-api` integration fixtures call the process-global `std::env::set_current_dir` while the binary's tests run in parallel threads. When one test's `TempDir` drops, another test's in-flight `GitSessionGraphStore::repo()` hits `init_bare: Could not obtain the current directory`. `lattice-session/src/test_utils.rs` documents exactly this hazard and retries from a known-good CWD; `git_graph/mod.rs`'s `init_bare` path has no equivalent recovery.
- **Current status**: OPEN, pre-existing, low impact. Observed roughly 1 run in 3 of `cargo test --workspace` on a loaded 24-core dev machine; **not** reproducible per crate — `cargo test -p lattice-session` and `-p lattice-api` passed 8/8 and 10/10 consecutive runs, and the sanctioned `cargo xtask ci` gate runs per crate (see `ENV-OOM-01`), so CI is unaffected.
- **Plan**: (a) give the git-graph fixtures a `TempDir` root, or extend the `DirectoryExists` recovery to the top-level match; (b) either drop `set_current_dir` from the API fixtures (they already pass `test_project_root` explicitly) or serialize them behind the same global env lock `ScopedXdgStateHome` uses. Until then, prefer `cargo xtask ci` over `cargo test --workspace` locally, and `rm -rf /tmp/lattice-session-git-graph-tests` between full-workspace runs.

- **收口（2026-07-27）**: 两条 race 均已修复并落地 main —— (a) git-graph fixture 改用 `TempDir` 唯一路径（#106，收口 `L-47-02`）；(b) `lattice-api` fixture 的多余 `set_current_dir` 被删除，生产 `init_bare` 路径补上 CWD 恢复（#109）。本条保留为记录，不再是 OPEN。

### L-39-01: Local Workspace 读/观察路径仍是 `resolve_secure`-only,只有写路径钉住了 dirfd — `security` / `lancet-native` / GitHub #39
- **位置**: `crates/lancet-runtime/src/workspace/local.rs`(#30 从 `lancet-native` 下移;路径别名不变语义):`resolve_secure`(约 L88 起)与其调用方 `read_file` / `list_dir` / `grep` / `find_files`;`open_parent_dir_secure`(约 L244 起)目前只服务 `create_excl` / `install_atomic` / `remove_file`。
- **问题**: #39 只把**写**路径改成 pinned-dirfd(`openat`/`renameat`/`unlinkat`,`O_NOFOLLOW|O_DIRECTORY`)。读/观察路径仍是「逐段 `symlink_metadata` 校验 → 拿最终 `PathBuf` 去 `File::open`」的两段式模型,校验与打开之间存在 TOCTOU 窗口:本地攻击者在 resolve 之后把某个父组件换成 symlink,可以把一次 **READ** 重定向到 workspace root 之外。写路径的同类窗口已闭合,读路径没有。
- **当前状态**: ACCEPTED RESIDUAL。威胁模型要求攻击者已对项目目录有并发写权限;此时读被重定向只泄露内容,不改写 Local bytes,而唯一能持久改变宿主状态的写路径已结构性闭合。项目根在构造时 canonicalize,逐段 `symlink_metadata` 仍然挡掉所有**静态**存在的 symlink 组件。
- **升级条件**: 若 Local Workspace 需要在多租户 / 不可信共存目录下运行,或读取结果被当作安全决策输入(例如把读到的内容喂给 policy) → 升为 MEDIUM。
- **计划**: 复用现成的 `open_parent_dir_secure` + `dir_open_flags()` 把叶子 fd 交给读路径,`grep_walk`/`find_walk` 改为 `openat`-relative 递归。**不要**为此引入第二套路径解析实现——单一解析权威是 ADR 0003 的前提。

### L-39-02: `grep_walk` / `find_walk` 递归无显式深度上限 — `robustness` / `lancet-native` / GitHub #39
- **位置**: `crates/lancet-runtime/src/workspace/local.rs`(#30 从 `lancet-native` 下移;路径别名不变语义):`grep_walk`(约 L822 起)、`find_walk`(约 L949 起)。
- **问题**: 两者都是对 `fs::read_dir` 的直接递归,只在 `out.len() >= max_results` 与 `GrepBudget::exhausted()` 时提前返回。#39 引入的 byte budget(`GREP_MAX_FILE_BYTES` / `GREP_MAX_TOTAL_BYTES` / `GREP_MAX_LINE_BYTES`)限住了 IO 与内存,但**不限栈深度**:目录嵌套深度直接等于递归深度,`find_walk` 更是连 byte budget 都没有。
- **当前状态**: ACCEPTED RESIDUAL。当前 walk 不进入目录 symlink(见 L-39-03),因此不存在环路;真实深度被文件系统路径长度上限约束(Linux `PATH_MAX` 4096,每层至少两字节),在默认栈上不足以溢出。
- **升级条件**: 若 walk 改为跟随目录 symlink(环路可无限深)、或观察路径被搬到栈更小的 worker 线程 → 升为 MEDIUM。
- **计划**: 给两个 walk 加显式 `max_depth` 常量与参数,超限按「静默截断」处理(与 over-cap 文件同口径),不要报错。

### L-39-03: grep/find walk 对目录 symlink 既不进入也不报告,与 `list_dir` 口径不一致 — `api-shape` / `lancet-native`(pre-existing)
- **位置**: `crates/lancet-runtime/src/workspace/local.rs`(#30 从 `lancet-native` 下移;路径别名不变语义):`grep_walk` / `find_walk` 中的 `fs::symlink_metadata` + `meta.is_dir()` / `meta.is_file()` 分支;对照 `list_dir` 的 `file_type_kind`。
- **问题**: symlink 条目的 `symlink_metadata` 既不是 dir 也不是 file,两个 walk 的 if/else-if 因此整条跳过。不递归是符合 root-escape 防御预期的,但**也不出现在结果里**——哪怕 `find_files` 的 pattern 精确匹配它的文件名。#39 之后 `list_dir` 会把同一条目报成 `PathKind::Symlink`,于是同一个 symlink 在 list 与 find 下有两种可见性答案。
- **当前状态**: ACCEPTED RESIDUAL,pre-existing——该行为早于 #39,#39 只修了 `list_dir` 一侧,没有(也不打算在本票)统一 walk 一侧。
- **升级条件**: 若用户报告 `find_files` 漏掉可见 symlink,或 Policy-Visible Workspace 要求 read/list/grep/find 四条路径给出同一可见性答案 → 升为 LOW-MEDIUM。
- **计划**: 让两个 walk 复用 `file_type_kind` 分类:visibility 允许时把 symlink 条目**报告**出来,但仍然不递归进入。

### L-39-04: pinned-dirfd 写路径挡不住"把 pinned 目录整体 rename 出工作区" — `security` / `lancet-native` / GitHub #39
- **位置**: `crates/lancet-runtime/src/workspace/local.rs`(#30 从 `lancet-native` 下移;路径别名不变语义):`open_parent_dir_secure`(约 L244 起)与三个写入调用方 `create_excl` / `install_atomic` / `remove_file`。
- **问题**: #39 的 pinned-dirfd walk 分两步——先解析父目录拿到 fd,再对叶子做 `openat`/`renameat`/`unlinkat`。`openat2(RESOLVE_BENEATH)` 是把父与叶子在一次内核调用里原子解析,本实现不是。攻击者在 pin 与叶子操作之间把**被 pin 的目录本身** rename 到工作区外,fd 跟随 inode,叶子就落在工作区外。整个过程没有任何 symlink,`O_NOFOLLOW` 不会触发,且 pin 之后没有任何 containment 复检。本分支的对抗性复核用一份 C 复刻实测确认:pin 住 `root/a/b` 后把 `root/a/b` 移出工作区,create 落到 `.../outside/stolen/victim.txt`。
- **当前状态**: ACCEPTED RESIDUAL。**#39 报告的原缺陷已闭合**——"父目录组件在校验后被替换为 symlink"这一向量被 pin 结构性挡住。本条是复核新发现的、更窄的另一向量:要求攻击者已对项目目录持有并发写权限(与 L-39-01 同一威胁模型前提)。相关 doc comment 已就地改写,不再声称 `RESOLVE_BENEATH` 等价。
- **升级条件**: 若 Local Workspace 要在多租户 / 不可信共存目录下运行,或写路径被用于安全敏感落盘(凭据、策略文件)→ 升为 MEDIUM。
- **计划**: 在 Linux 上改用 `openat2(RESOLVE_BENEATH)` 对整条路径做原子解析并替换当前 pinned walk。**不要**用"pin 后沿 `..` 走回 root 比对 dev/ino"来充数——那本身仍是 TOCTOU,只是把窗口变窄,会带来已闭合的错觉。

### L-39-05: grep 返回载荷无聚合上限,`GREP_MAX_LINE_BYTES` 只约束单行 — `robustness` / `lancet-native` / GitHub #39
- **位置**: `crates/lancet-runtime/src/workspace/local.rs`(#30 从 `lancet-native` 下移;路径别名不变语义):`GREP_MAX_LINE_BYTES` 及其 doc comment;`crates/lancet-runtime/src/host_workspace.rs` 的 budget charge(#30 前为 `lancet-native/src/handlers/observation.rs` 的 `charge_observation_bytes`,已随迁移删除);`crates/lancet-authority/src/policy/observation.rs` 的 `max_grep_matches`(默认 1000)。
- **问题**: #39 的 byte budget 约束的是**读入**字节数,不约束**返回**载荷。单条匹配行上限 64 KiB × 默认 1000 条上限 ≈ 64 MiB 可以在 `charge_observation_bytes` 被调用之前就堆在 `out` 里。复核实测该形状峰值 RSS 66,604 KB。`GREP_MAX_LINE_BYTES` 的 doc comment 声称"单条病态长行无法撑爆 GrepMatch 载荷"——按单行成立,按聚合不成立。
- **当前状态**: ACCEPTED RESIDUAL。相对原缺陷(单文件 200 MB → RSS +400 MB,且与 `max_results` 无关)已是数量级改善,且现在有确定性上界(64 KiB × `max_results`),不再随文件大小无界增长。
- **升级条件**: 若 `max_grep_matches` 默认值调高,或 grep 结果进入长驻内存 / 并发多路 → 升为 MEDIUM。
- **计划**: 给 `out` 加聚合字节预算(命中即停,与 over-cap 文件同口径静默截断),并把 `GREP_MAX_LINE_BYTES` 的 doc comment 改成只声称单行上界。

### ENV-DISK-01: 多 worktree 并行编译撑爆磁盘,`xtask ci` 逐 crate 超时/配额失败(非代码问题)— `environment` / dev-loop
- **位置**: 开发机环境,非 repo 代码。`xtask/src/ci.rs` 的 per-step 超时(`LATTICE_TEST_TIMEOUT_SECS`,默认 300s)与 cargo 全局包缓存锁 `~/.cargo/.package-cache` 是放大器。
- **问题**: 2026-07-27 一次 `cargo xtask ci` 报 `cargo test failed for 3 crate(s): lancet-native, lattice-api, lattice-tui`。逐条查明**全部为环境所致,无一为代码回归**:(1) `lancet-native` 与 `lattice-api` 是 `timed out after 300s` —— 另一个 Claude 会话正在 `issue-28-cell-capability` worktree 里跑 `cargo test`,两边争用**全局** `~/.cargo/.package-cache` 锁,反复 `Blocking waiting for file lock on package cache`,编译阶段被拖过 300s 硬超时;(2) `lattice-tui` 是 rustdoc 写临时文件失败 `Os { code: 122, kind: QuotaExceeded, message: "Disk quota exceeded" }`。实测:根分区 938G 已用 880G(**95%**),`/tmp` tmpfs 16G 仅剩 4.8G;lattice 各 target 目录合计约 **217 GB**(主仓 108G + `issue-28-cell-capability` 38G + `issue-39-audit-sink` 37G + `issue-64-rewind` 34G)。三个 crate 在串行、低并发(`CARGO_BUILD_JOBS=2`)下单独重跑全绿:lancet-native 155、lattice-api 136、lattice-tui 3。
- **当前状态**: ACCEPTED RESIDUAL(环境侧)。与 ENV-OOM-01 同源——常驻多 worktree + 并行会话超售本机资源;那一条是内存,这一条是磁盘与全局 cargo 锁。Linux CI 使用单 worktree、独占 runner,不受影响。
- **升级条件**: 若在**无**并行会话、磁盘充裕时仍出现 300s 超时或配额失败 → 升为 MEDIUM 并排查是否有测试真的变慢。
- **计划**: (a) 跑门禁前先确认没有其他会话在编译,或用 `LATTICE_TEST_TIMEOUT_SECS` 抬高超时;(b) 定期 `cargo clean` 不活跃的 worktree target(单个 34–38 GB);(c) **不要**因为这条去删别人正在用的 worktree target;(d) 若要根治并行争用,给各 worktree 配独立 `CARGO_HOME`(锁即不再全局),代价是依赖缓存不复用、磁盘占用更高——在当前 95% 占用下不划算。

### ENV-WASM-01: 不过滤的 `cargo build --workspace` 在本开发机 host-link wasm guest cdylib 失败 — `environment` / dev-loop(pre-existing)
- **位置**: `tools/{fetch-web,mutate-create,mutate-edit,noop,observe-workspace,todo-write}`——六者都是 `[lib] crate-type = ["cdylib"]`、真实目标 `wasm32-unknown-unknown`，且都是根 `Cargo.toml` 的 workspace member；任何不排除 guest crate 的 `cargo build --workspace`。
- **问题**: unfiltered workspace build 会拿**宿主**目标去链接这些 wasm guest cdylib，并因 wit-bindgen 导出的 Component 符号不能进入 Linux host version script 而失败。它们只在 `xtask tool-build` 的 wasm 目标下才有意义。
- **当前状态**: ACCEPTED RESIDUAL，非 #34 回归。#30–#32 把五个默认 Source Tool 全部迁成 Component 后，原先只影响 `noop-echo` / `observe-workspace` 的宿主链接限制自然覆盖全部六个 guest crate。CI 与 `xtask ci` 的 fmt/clippy/test 门不 host-link 它们；`xtask ci` 另以 `cargo xtask tool-build --verify` 构建并验证真实 wasm32 artifacts。
- **升级条件**: 若任何门禁步骤引入不排除 guest crate 的 host `cargo build --workspace` → 升为 MEDIUM。
- **计划**: host 构建验证使用 `cargo build --workspace` 并逐一 `--exclude` 六个 guest cdylib；guest 构建使用 `cargo xtask tool-build --verify`。若要根治，使用独立 guest workspace 或等价的 Cargo 目标隔离；**不要**为让宿主链接通过而改它们的 `crate-type`——`.wasm` 产物的字节可复现性依赖当前配置（见 L-5-02）。

### L-39-06: `lattice audit --tail` 在 turn 进行中可能漏行;多批次分页无黑盒覆盖 — `robustness` / operator-path(pre-existing)
- **位置**: `crates/lancet-audit/src/operator.rs`:`tail_audit` 批次循环的游标(每批 `since_audit_id = since + 1`);覆盖面对照 `crates/lattice-system-harness/tests/{fast_audit_cli,direct_operator}.rs`。
- **问题**: 两条,同源于 tail 的游标语义。(1) **漏行**:某次 call 若在低 `audit_id` 上 `start_call`、而在游标已越过该 id 之后才 `finalize_call`,它永远不会被 emit——`query_rows` 只返回 finalized 行,而游标不会回退。并发测试只断言"批内无重复 `audit_id`",从不断言"无遗漏"。这满足验收要求的"排除未 finalize 行",但不满足操作员可能默认的"`--tail` 会抽干一切"。(2) **黑盒缺口**:多批次分页只在 `lancet-audit` 单元层覆盖;黑盒侧 `direct_operator` 的 tail 断言是 2-turn store 上的 `tail_rows > 0`(单批次),`fast_audit_cli` 只走缺失 store 路径。RFC #41 禁止 harness 直接植入内部 audit DB,而真实 turn 不会产生 >1000 行,所以黑盒无法自然覆盖。
- **当前状态**: ACCEPTED RESIDUAL。(1) 与 HEAD 逐字节一致,非 #39 引入;#39 只是把行为搬了个位置。(2) #39 期间曾加过一条 CLI 层多批次测试,因与账本既有判决(`audit_tail_streams_rows_for_seeded_db` = 删除)冲突且只能断言 `Ok`(`run_async` 直接打印 stdout,拿不到 row count)而移除;真实契约由 `operator::tests::tail_audit_paginates_large_result_without_cep_server` 及本次新增的两条边界测试(空批游标回退、恰好 `AUDIT_TAIL_MAX_ROWS` 单批次)持有。复核已手工验证 1001 行经真实 `lattice` 二进制得到 rows 1001 / batches 2 / done true。
- **升级条件**: 若 `--tail` 被接入长驻 streaming 消费方(漏行会累积)、或 `render_tail_output` 需要 CLI 侧自有分页/截断逻辑(那时黑盒缺口变成真实回归面)→ 升为 MEDIUM。
- **计划**: (a) 在 `tail_audit` doc comment 里显式写明"turn 进行中不保证完整抽干,完整性以 turn 结束后重跑为准";(b) 若要覆盖 CLI 侧,把渲染从 `println!` 抽成对 `impl Write` 的函数,再对渲染输出而非 `run_async` 的返回值断言。

### L-39-07: `OperatorAuditError::Config` 与 `LancetError::Config` 的 Display 前缀不一致 — `api-shape` / `lancet-audit` / GitHub #39
- **位置**: `crates/lancet-audit/src/operator.rs`(`#[error("{0}")]`)对照 `crates/lancet-api/src/error.rs`(`invalid lancet configuration: {0}`)。
- **问题**: 逐字透传的 Display 正是"经门面映射后错误文本逐字节不变"的实现手段,当前在树内成立(所有消费方都走 `lancet-api` 门面)。但 #39 之后同一行为多了第二个入口:直接调 `lancet_audit::query_audit` 的未来调用方会打印不带 `invalid lancet configuration: ` 前缀的文本。
- **当前状态**: ACCEPTED RESIDUAL,非兼容性破坏——树内零个直连消费方,`fast_audit_cli` 对 stderr 的断言仍然命中。
- **升级条件**: 若出现绕过 `lancet-api` 门面直连 `lancet_audit::operator` 的生产消费方 → 升为 LOW-MEDIUM,届时需决定前缀归属。
- **计划**: 维持逐字 Display(它保证了本次迁移的字节级兼容);若将来出现直连消费方,由该消费方自己决定前缀,不要改 `OperatorAuditError`。

### T-39-01: `docs/TEST-TRIAGE.md` 早期批次说明的 running total 与 machine ledger 相差 2 — `documentation` / test-triage
- **位置**: `docs/TEST-TRIAGE.md` §1.9「#46 residual elevate closeout」的 `counts 变更`(散文写「删除 211->214」);对照 `docs/test-triage-decisions.json` 与 §1 汇总表、§1.5 决定分布表。
- **问题**: 各批次说明里的「全量」running total 是逐批手算累加的,与 machine ledger 的真实计数存在 **2 条固定偏差**:#39 改动前 JSON 的实际基线是 keep 1191 / elevate 42 / **delete 216**,而 §1.9 散文写的是 `211->214`。汇总表(§1、§1.5)一直对齐 JSON,只有批次说明的 running total 漂移。
- **当前状态**: ACCEPTED RESIDUAL,pre-existing——偏差在 #39 之前就已存在,#39 未引入也未放大。#39 新增的 §1.12 `counts 变更` 显式使用 JSON 真实基线(216->217)并就地标注了这 2 条偏差。历史批次说明属 append-only 执行记录,不回写。
- **升级条件**: 若有人把批次说明的 running total 当成权威计数来源(而非把 JSON / 汇总表当权威)→ 升为 MEDIUM 并一次性重算所有批次说明。
- **计划**: 不回写历史批次说明。今后新增批次说明一律引用 JSON 真实基线,并保持 §1 汇总表、§1.5 决定分布表、按 crate 表与 JSON 逐条一致(可用一次性脚本重新派生校验)。


（L-47-01 与 L-47-02 已于 2026-07-27 收口，见下方 OBSOLETE / CLOSED 段与 FIX.md `#105 / #106`。）


### L-5-02: #5 slice B toolchain-authority divergences + cross-machine reproducibility unverified — `tooling` / #5 slice B / #73
- **位置**: `tools/noop/manifest.toml` (`toolchain = "rustc-1.96.0+wit-bindgen-0.59+wasm-tools-1.254.0"`); `xtask/src/tool_codegen.rs` (WIT `,` separators); `tools/noop/src/lib.rs` (direct `wit-bindgen` dep, `export_tool!` invoked inside `mod bindings`); `crates/lattice-tool-prelude/src/lib.rs` (`export_tool!` doc); `xtask/src/tool_build.rs` (`build_guest` 的双 `--remap-path-prefix`)。
- **问题**: #5 slice B diverged from the plan sketch where the actual installed toolchain is authoritative: (1) WIT record fields use `,` not the plan's `;` (wasm-tools 1.254/wit-bindgen 0.59 grammar); (2) `wit-bindgen` is a direct guest dep because `generate!` emits `wit_bindgen::rt::...` by bare crate path a prelude re-export cannot satisfy (plan said guests never depend on it directly); (3) `export_tool!` is invoked inside `mod bindings` because wit-bindgen's `export!` resolves `self` at the call site; (4) toolchain fingerprint records wit-bindgen-0.59 not 0.57. Separately, `.wasm` bit-for-bit reproducibility was verified only on this machine (`tool-build --verify` reproduces 6fcc7fc8...); cross-machine/cross-build reproducibility is unverified and the `--verify` lane is intentionally warn-only (decision 1) until proven stable.
- **当前状态**: ACCEPTED RESIDUAL (by plan); tracked by **#73**. All divergences are toolchain-fact-driven, not design regressions; every #5 slice B acceptance criterion is met. The `--verify` warn-only lane collects reproducibility data without blocking. Slice C already embeds the committed `.wasm`; hard-gate promotion still waits on cross-machine bit-for-bit evidence.
  **2026-07-27 部分推进（#80，PR #104/#109）**：跨机可复现性的两个**结构性阻碍**已消除，但跨机证据本身仍未取得。已做：(a) 发现并修复 committed `.wasm` 里烧进维护者主机 rustup sysroot 绝对路径的问题 —— `build_guest` 原先只 remap workspace root，`core/src/str/pattern.rs` 因此以 `/home/<user>/.rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/...` 形式入档，而同文件其余 std 路径已被 rustc 规范化为 `/rustc/<commit-hash>/`；现补第二条 `--remap-path-prefix` 把 sysroot 映射到同一前缀，`strings` 核验宿主路径归零，digest 由 `6fcc7fc8...` 变为 `b677a9b3...`。**在此之前任何跨机 `--verify` 都必然误报漂移**，#73 的验收标准结构上无法达成。(b) full-gate 的 verify 步骤补 `if: always()`（原先排在最易失败的 rollout 之后，rollout 一失败就永久收不到证据）并把输出 tee 到 `target/rollout/component-verify.log` 以进 artifact 上传（原先只在 job stdout，取不回来）。(c) `[artifact] toolchain` 指纹补入 `wasm-tools-1.254.0` 并加门禁测试锁定三处（常量／full-gate 安装器／manifest 指纹）一致。**未做**：真正的跨机 bit-for-bit 证据 —— 需 nightly full-gate 实跑至少一次并与本机 digest 比对；在此之前 warn-only 不可升级为硬门禁。
- **计划**: Own in #73. Keep the actual toolchain authoritative; do not back-fit the plan's `;`/0.57 sketch. 下一步是等一次 nightly full-gate 跑出 `component-verify.log`，把 runner 上的 rebuilt digest 与本机 `b677a9b3...` 比对：一致则按 decision 1 escalation 把 `tool-build --verify` 升为硬门禁；不一致则以该 log 为据定位剩余的不确定性来源（不要再假定只剩路径问题——sysroot 一条已排除）。若最终证明存在无法消除的差异，则文档化显式的非 bit-for-bit 替代方案。Revisit the wit-bindgen direct-dep / `export_tool!`-inside-bindings pattern if a later wit-bindgen release restores a re-export-only or root-callable export seam.




### L-11-01: `tail_audit` 批次循环有 `yield_now` 但无专用 `CancellationToken` + CLI 接线 — `robustness` / operator-path
- **位置**: `crates/lancet-audit/src/operator.rs`（`tail_audit` 批次循环,`tokio::task::yield_now().await` 在 L279;循环体 `query_rows` 同步）。GitHub #39 起该行为下沉到 `lancet-audit`;`crates/lancet-api/src/audit.rs` 仅剩一行委托,不再是本 finding 的位置。
- **问题**: 批次循环底部已加 `tokio::task::yield_now().await`,drop-future 取消可在批次边界生效。但无专用 `CancellationToken` 从 CLI 接入 `tail_audit`,`lattice audit --tail` 的 Ctrl-C 仅靠默认 future-drop,无显式信号接线。退役前的 native handler 经 `HandlerContext { cancellation: Cancellation }` 有 per-batch watch 检查;direct-store 路径用 `yield_now` 兜底但未做专用 token + CLI 接线。
- **当前状态**: ACCEPTED RESIDUAL(downgraded from LOW-MEDIUM → LOW)。已做:`yield_now().await` 批次间 yield(取消可在批次边界生效)。未做(最优处理):专用 `CancellationToken` + `lattice audit --tail` Ctrl-C 信号接线。最坏工作量由 `AUDIT_TAIL_MAX_BATCHES × AUDIT_TAIL_MAX_ROWS`(1000 × 1000)硬上限兜底;超限返回 `LancetError::Config` 并带 `last_audit_id` 供 `--since` resume。`--tail` 是操作员路径,典型 store 量级毫秒级完成。`tail_audit` doc comment 已显式写明该降级契约。
- **升级条件**: 若 audit store 量级进入百万行常态、或 `--tail` 被接入长驻 UI/streaming 消费方需要即时取消响应 → 升为 MEDIUM 并加 `CancellationToken` + CLI 信号接线。
- **计划**: 暂不实现专用 token + CLI 接线;维持 `yield_now` + 批次上限兜底 + 显式契约文档。

### L-11-02: 活跃 turn 并发读取仅由合成 writer 测试覆盖,无 live-turn 集成测试 — `verification` / concurrency-invariant
- **位置**: `crates/lancet-audit/src/operator_tests.rs`(`operator_audit_reads_concurrent_with_active_writer`,约 288–399 行);降级契约文档在 `crates/lancet-audit/src/operator.rs` module doc;`crates/lancet-audit/src/audit.rs`(`query_rows` SQL 谓词 + `init_schema` WAL)。GitHub #39 起测试与契约文档均已离开 `crates/lancet-api/src/audit.rs`。
- **问题**: 并发读不变量(读不阻塞 writer、只返回 finalized 行、单次读 snapshot 一致)由结构性保证(SQL `ended_at IS NOT NULL AND result_or_error IS NOT NULL` + WAL 语句级 snapshot)+ `operator_audit_reads_concurrent_with_active_writer` 回归测试覆盖。但该测试的 writer 是**合成**的 —— 直接调 `CepAuditStore::start_call`/`finalize_call`,不 spawn 活跃 `LancetRuntime`/`stream_turn`。无 live-turn 集成测试验证"真 turn 在另一 task 写 audit store 的同时从并发 task 调 `query_audit`/`tail_audit`"的端到端并发读行为。
- **当前状态**: ACCEPTED RESIDUAL(downgraded from MEDIUM → LOW)。合成 writer 测试在 store 并发边界(SQLite WAL + finalize 谓词)上与 live turn 等价 —— live turn 最终也走同一 `CepAuditStore` API,无额外并发 hazard —— 故接受合成 writer 测试为充分兜底(downgrade),不补 live-turn 集成测试(最优处理)。`operator.rs` module doc 已显式写明该降级契约。
- **升级条件**: 若 audit 写路径引入 `CepAuditStore` 之外的并发 hazard(如跨进程写、外部锁、非 WAL journal mode)、或 `query_rows` 改为跨多次 `SELECT` 拼接而中间释放 snapshot → 升为 MEDIUM 并补 live-turn (`LancetRuntime`/`stream_turn`) 并发读集成测试。
- **计划**: 暂不补 live-turn 集成测试;维持合成 writer 测试 + 结构保证 + 显式契约文档。

### L-21: 工具执行 `await_with_deadline` 未与 `cancel_flag` 竞争 — `robustness`
- **位置**: `crates/lattice-agent/src/agent_loop/mod.rs`（`execute` 调用处）
- **问题**: 用户取消后，须等当前工具跑完或触及 turn `deadline` 才能在 checkpoint #5 退出；长时 `fetch.web` 或大范围 workspace observation 可能造成取消滞后。任意命令执行已退役，不再是此 finding 的场景。
- **当前状态**: Plan 15-02 **D-07** 明确不 abort in-flight 工具。
- **升级条件**: 若 bounded observation 或网络 timeout 仍不能把最坏取消延迟控制在可接受范围，**升为 Medium**。
- **计划**: `select!` cancel + 工具 future，并把现有 CEP cancellation 传到可中断的 native observation/fetch 路径；不要为取消机制引入子进程执行。

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

## P22.5 supervision verification (2026-05-28)

Live-scenario harness: `benchmarks/lancet-dogfood/legacy/p22-real-scenarios.sh` (historical; disposable workspaces under `build/tmp/p22-real-scenarios/`).

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

Self-host harness: `benchmarks/lancet-dogfood/legacy/p22-self-host.sh` (historical; disposable clone under `build/tmp/p22-self-host/<run-id>/lattice-under-test`).

| Task | Default | Result | Notes |
|------|---------|--------|-------|
| task-a | yes | PASS | Doc-only `docs/p22-self-host-note.md`; verify `echo self-host-doc-ok`; no `crates/` diff |
| task-c | yes | PASS | Out-of-root canary read denied; `GuardDenied` on `..` escape; `ReplanRequired` on forbidden `crates/` write |
| task-b | opt-in (`P22_SELF_HOST_TASK=task-b`) | PASS (optional) | Fixture `tests/fixtures/p22-self-host/fixture-crate/`; verify `cargo test --manifest-path …` |

**Pass criteria:** outer checkout must not change; inner agent only mutates declared scope (or shows `ReplanRequired` / guard denial). Report: `build/tmp/p22-self-host/report.md`. Default verification is **task-a + task-c** from clean supervisor checkout with live `compat:deepseek-v4-pro` (2026-05-28). Task B is implemented but not part of the default merge gate.

---

## OBSOLETE / CLOSED

（完整修复记录见 [FIX.md](./FIX.md)。）

### M-REWIND-01: ~~Local mutation history and Rewind were missing~~ — CLOSED by #63/#64
- **Resolution:** `lancet-runtime::WorkspaceHistory` records successful atomic mutations in private Git; `lancet-api` and `lattice-api` coordinate verified restore and ref movement. Residual policy choices remain in `L-64-01`.

### L-GH112-01: ~~evidence publication could fail independently of the invocation terminal~~ — CLOSED by #139-B/#34
- **Resolution:** terminal material and `CellEvidenceTriple` now publish atomically in the Git `InvocationStore`. The deleted SQLite side-write and shape-check paths cannot recur.

### L-GH29-01: ~~`max_todo_ops` was parsed but not enforced after Component migration~~ — CLOSED by #34
- **Resolution:** the dead profile/external-policy field, tracker counter, snapshot fields, and charge API were deleted. `session.todo_write` retains its manifest-owned per-call schema bound; Tool enablement and denial use the generated catalog and Cell capabilities.

### L-38-01: ~~the scaffold feature did not compile~~ — CLOSED by #34
- **Resolution:** the scaffold product/test surface and its feature were deleted with the migration layers; no tombstone test remains.

### L-5-01: ~~Cell and legacy impact/effect vocabularies coexisted~~ — CLOSED by #34
- **Resolution:** production policy and execution now use only manifest-generated Cell scopes/capabilities and versioned Cell evidence. The legacy effect types and policy machinery were deleted.

### L-GH139B-01: ~~production provider adapters bypassed `provider_projection`~~ — CLOSED by #139-B
- **Resolution:** `ChatRequest.messages` now carries `NativeMessage<BoundToolCall>` directly. Every production provider request builder delegates message and ToolResult encoding to `request_messages_to_wire`; every SSE parser accumulates one provider-complete response and publishes `MessageComplete` through `wire_to_native`, while all usage is parsed through `wire_usage` into the single `lattice-conversation::TokenUsage`. `ModelStepDriver` no longer assembles protocol messages, Tool IDs, or wire usage. Provider call IDs remain optional; Gemini `tc_*` synthesis, the legacy `Message`/`Role` request DTOs, and the buffered response assembler are deleted. A production-source boundary regression requires all three adapters to call the shared request, response, and usage projectors.
- **Verification:** `cargo test -p lattice-llm` (81 unit + 34 official-wire projection tests), `cargo test -p lattice-agent` (12 unit + 5 streaming-seam tests), `cargo test -p lattice-api`, workspace all-target compilation, and the #139-B black-box privacy/replay/crash gates. Official OpenAI finish→usage-only→`[DONE]`, malformed usage/arguments, OpenAI and Gemini multi-chunk accumulation, Anthropic/Gemini opaque replay state, optional/missing IDs, parameterless Gemini calls, same-provider exact replay, and cross-provider non-leakage are pinned on the production seam.

### L-47-01: harness SIGINT 广播到测试进程自身的进程组，使 ubuntu CI 以 exit 130 中断 — `robustness` / harness / #47 / #105
- **当前状态**: 2026-07-27 closed — 由 **#105** 修复。`driver.rs` 现在用 `libc::getpgid(child_pid)` 从内核回读实际 pgid（不再假定等于 `child.id()`），任何组信号发送前先用 `libc::getpgrp()` 比对并在命中自身组时 fail-closed，且改用 `libc::killpg` 直接投递而非 spawn `kill(1)`（消除负号参数在不同 `kill` 实现间的解释差异）。新增两条回归测试锁定该契约；`direct_failure_sigint_while_stalled_commits_cancelled` 保留并通过，SIGINT 协作取消覆盖未削弱。**CI 实跑确认（2026-07-27）**：ubuntu lane 已转绿。同一对上游提交（`44ceabc1` + `38af35bb`）在 PR #104 上 ubuntu-latest 仍以 exit 130 失败（run 30222133081，macos 通过），而叠加本修复的 PR #107 ubuntu-latest 与 macos-latest 双双通过（run 30257981171）；两次运行只差这一个提交，坐实信号源在 harness 内且已消除，无需启用 #105 计划第 4 步的 `#[ignore]` + serial lane 退路。完整记录见 FIX.md `#105 / #106`。

### L-47-02: `git_graph` 测试 fixture 使用固定共享路径 — `robustness` / test-isolation / #47 / #106
- **当前状态**: 2026-07-27 closed — 由 **#106** 修复。`temp_dir()` 改为返回持有 `tempfile::TempDir` 的 `Fixture` 守卫，路径唯一且 drop 时自动清理，`name` 退化为可读前缀；删除手写 `remove_dir_all` 前置清理，30 个调用点改用 `dir.path()`。反向验证：并行两份 `CARGO_TARGET_DIR` 各 29 passed / 0 failed（修复前有 2 个失败）。完整记录见 FIX.md `#105 / #106`。

### T-24-01: Direct-Local observation root-escape acceptance coverage — `verification` / #24
- **Current status**: 2026-07-23 closed — `direct_observation` now drives traversal and absolute outside-root paths through read/list/grep/find (8 calls), requires public `GuardDenied` CEP finishes, exact denial text, no canary leakage, unchanged outside bytes, and a completed Turn. Focused and full harness runs passed.
- **Update (2026-07-31, #30)**: the expected error code for the same 8 escape calls changed `GuardDenied` → `PathContainment`. The denial used to come from the legacy policy hook (which normalized containment failures to `GuardDenied`); the Component Tool's refusal now comes from the Tool Host's own path resolution, which reports `PathContainment` — the same code the native workspace layer already used for a containment failure it detected itself. The scenario's other assertions (exact denial text, no canary leakage, unchanged outside bytes, visibility/bounds matrix) are unchanged.

### T-24-02: stale mutation `ReplanRequired` identity — `verification` / #24
- **Current status**: 2026-07-23 closed — the stale-edit scenario now pins public `cep_call_finished` identity (`method=mutate.edit`, `ok=false`, `error_code=ReplanRequired`) while retaining generic Tool failure, external-byte preservation, and same-session reopen visibility. Focused and full harness runs passed.

### L-24-03: Direct-Local integration-test helper duplication — `maintainability` / #24
- **Current status**: 2026-07-23 closed — shared harness-only plumbing moved into private `tests/support/mod.rs` across all nine Direct-Local integration targets; scenario-specific assertions remain local, no product/public API or dependency was added, and the full harness plus all-target clippy passed.

### T-23-01: #45→#46 Direct-Local assertion-level handoff reconciled — `test-refactor` / #72
- **位置**: `docs/TEST-TRIAGE.md` §1.5/§2；`docs/test-triage-decisions.json`（固定 1449 条 identity 快照）。
- **问题**: #46 完成记录把 44 条 handoff 同时概括为“20 条严格超集删除 + 20 条半保留”，与实际 slice 记录、machine ledger 和 assertion-level 证据矛盾；20 条残余 `planned_direct_*` 需要最终逐条判决。
- **当前状态**: 2026-07-24 closed — #72 逐条复核 44 条 handoff 为 **keep 14 / elevate 3 / delete 27**；#46 residual closeout 随后将残余 3 条 elevate 落地为 exact external scenarios 并删除内部测试，handoff 终态 **keep 14 / elevate 0 / delete 30**。machine/prose ledger 同步为 keep 1193 / elevate 42 / delete 214，layer 仍为 local 1372 / fast 45 / Direct-Local 32，总 identity 仍为 1449。完整记录见 FIX.md GitHub #72 与 #46 residual。

### T-23-03: 158 条 `lattice-workspace` keep 行指向已随 #40 删除的 crate — keep 决定待重审 - `test-refactor` / #23 / #24
- **当前状态**: 2026-07-19 closed — resolved by owner decision (option a)。158 行全部补记 delete（reason 对齐切片 3 的 11 条 OBSOLETE 口径：「#40 已删除 lattice-workspace crate（Upper/MSB 产品面），对应测试已不存在；补记 delete，不向 #24 虚构 replacement」），replacement 置 `n/a`，identity/target_layer(local_invariant) 不变。计数基线 keep 1337→1179 / elevate 86 / delete 26→184（总 1449 不变）；§1 汇总、按 crate 表、§1.5 决定分布与 lattice-workspace 小节 158 行全部同步。全量 1449 行 prose↔ledger 一致性校验零 mismatches，19 个 crate 计数行全对齐。完整记录见 FIX.md 对应条目。

### T-23-02: #23 review-fix - triage 映射精确化与 keep/delete 偏差 — `test-refactor` / #23
- **当前状态**: 2026-07-19 closed — fast/status/branch 工作已由 #23 切片 1–2 完成（`fast_lancet_cli` / `fast_session_cli` external-binary scenarios 落地，对应内部测试删除）；round-6 代码与账本已落地；MSB/KVM lane 已于 #40 退役。剩余 44 条 Direct-Local assertion-level 工作由 T-23-01 / #24 承接。完整记录见 FIX.md `#23` 切片 1–4 与 commit `8d6926d9`。

### M-LOCAL-01: ADR 0003 direct Local Workspace execution is not implemented — `architecture` / `lancet-native` / `lattice-api`
- **当前状态**: 2026-07-17 closed — implemented. Workstream A (#37 → #38 → #39 → #40) is complete. Production Turns operate directly on the canonical Local Workspace through the concrete guarded `LocalWorkspace` in `lancet-native` (#37); the `ExecutionBackend`/routing seam is deleted (#38); Policy-Visible Workspace read visibility is unified across read/list/grep/find (#39); and the Upper/MSB residue — profile `[workspace]`, `WorkspaceSection`/`EgressPolicyConfig`/`CredentialRef`, `lattice-workspace` crate, `microsandbox` dependency, MSB/KVM harness scenarios, `LATTICE_HARNESS_MSB` gate, `set_snapshot_lost`/`get_snapshot_lost` retained-snapshot chain, `lattice-session` promotion builder, and dead `SessionApi` wrappers — is deleted (#40, FIX.md `#40`). Direct-Local harness skeletons (plain-text turn, session reopen, policy denial) run in the ordinary lane with no KVM; the full external-binary scenario matrix is #24, testing-doc rewrite is #25, rewind history is #41/#42.

### M-33-01: Profile `[workspace]` → `UpperSpec` 翻译未在 Phase 33 规划 — `planning` / `v1.5-alpha`
- **当前状态**: 2026-07-17 closed — moot. #40 deleted the `[workspace]` profile schema (`WorkspaceSection`/`CredentialRef`/`EgressPolicyConfig`) along with the `UpperSpec` target type and `lattice-workspace`; there is nothing left to translate. Network access continues under `[lancet.network]` only; no egress/credential field was carried into the Direct-Local path. See FIX.md `#40`.

### M-33-02: WS-04 按项目 warm template 自动选择未接线 — `planning` / `v1.5-alpha`
- **当前状态**: 2026-07-17 closed — moot. #40 deleted `crates/lattice-workspace` including `template.rs`, warm-template naming/staleness, and the OCI rootfs/template cache; ADR 0003 has no per-project template concept. See FIX.md `#40`.

### M-33-06: Image 路径每次 create_upper 仍可能在 guest 内 apt-get 装 git（慢/脆/可超时）— `production-hardening` / `performance` / full-gate
- **当前状态**: 2026-07-17 closed — moot. #40 deleted the Microsandbox adapter, `deliver_baseline`/`APT_GET_GIT`, guest Git capability probing, git-ready templates, and the MSB/KVM harness scenarios that hit the 120s/300s hang. Production Turns no longer create any VM, so the guest apt-get path cannot recur. See FIX.md `#40`.

### M-33-03: 生产 adapter 上 WS-02 contract suite 非强制 — `verification` / `v1.5-alpha`
- **当前状态**: 2026-07-17 closed — moot. The production Microsandbox adapter this finding asked to harden was deleted in #40; the guarded Local Workspace is covered by temporary-directory tests (`lancet-native`) and the Direct-Local system harness instead. See FIX.md `#40`.

### M-33-04: Phase 33 计划/研究的 microsandbox SDK API 与锁定 crate 不符（已校正）— `planning` / `v1.5-alpha`
- **当前状态**: 2026-07-17 closed — moot. The `microsandbox` dependency and adapter whose SDK API this finding corrected were removed in #40; the historical lesson (verify SDK APIs against locked crate source) is retained in the overview plan. See FIX.md `#40`.

### L-21-02: `require_msb_env` 探针探的是 audit 路径，不验证 MSB stack 可用 — `verification` / harness / full-gate
- **当前状态**: 2026-07-17 closed — moot. #40 deleted `require_msb_env`, `layer_guard`, and the `LATTICE_HARNESS_MSB` gate entirely; there is no MSB stack left to probe, and the PTY smoke now runs Direct-Local in the ordinary lane. See FIX.md `#40`.

### L-34-02: Direct-mode `changed_paths()` porcelain enforcement weaker than Envelope snapshot diff — `security` / `defense-in-depth`
- **当前状态**: 2026-07-17 closed — moot. `changed_paths()` porcelain, `ExecutionBackend`, and `crates/lattice-workspace` (parser, WATCH anchors, production porcelain) were deleted across #38/#40. Direct-Local structured mutations are gated by exact per-target Guard L1 / System Path Exclusion / deny policy / Mutation Precondition / effect checks, not post-hoc Git diffs — the escalation condition (a mutation path bypassing those checks) is structurally excluded by the closed five-method catalog. See FIX.md `#40`.

### M-WS-01: Microsandbox cannot provide the proposed private CoW Turn Workspace — `architecture` / `lattice-workspace`
- **当前状态**: 2026-07-16 obsolete by decision, not fixed. Microsandbox 0.6.6 and CubeSandbox evaluation showed that projection plus checkpoint semantics were costly or mismatched. ADR 0003 supersedes ADR 0002 and removes the Turn Workspace requirement entirely; remaining implementation work is tracked by M-LOCAL-01. The capable-host projection/checkpoint probe remains historical evidence only and is deleted with the Microsandbox adapter.

### ENV-OOM-02: 并行测试暴露的 `unsafe env::set_var` 竞态 flake — `test-flakiness` / env-race
- **当前状态**: 2026-07-12 fixed。两处 flake 均压测复现后修复归零：(1) `layer_guard.rs` 两个 `MSB_ENV` 测试竞态（复现 232/2000 ≈12%）——加 `static MSB_ENV_LOCK: Mutex<()>` 串行化；(2) `lattice-api` `turn_runtime_constructs_one_adapter_per_turn` 的 `disk I/O error`（复现 8/500 ≈1.6%，错误路径落在别的测试的 `/tmp/.tmpXXXXXX` tempdir 下，坐实 XDG_STATE_HOME 竞态假设：第二次 `build_test_production_deps(persist_audit)` 经 `dirs::state_dir()` 无锁读 XDG，与持 `ENV_MUTEX` 的 `ScopedXdgStateHome` 邻居测试互踩）——给该测试加 `ScopedXdgStateHome`，同时把持久 audit DB 隔离进 tempdir（不再写真实平台 state 目录）。修复后同样压测循环 0/2000、0/500。这也解释了 FIX.md #20 条目记录的历史 `ci-local.sh` "disk I/O error" flake。完整记录见 FIX.md `ENV-OOM-02`（2026-07-12）。
- **残留（2026-08-09 复核后更新）**: 原记录指向 `lattice-api/tests/upper_routing.rs` 的 `upper_routing_e2e`——该文件已随 #40 删除，那条残留作废。`audit_turn_provenance.rs` 已随 #114 删除；当前 `crates/lattice-api/tests/` 下仍裸调 `set_var("XDG_STATE_HOME")` 的是 `release_e2e.rs:84`、`dogfood_impact_matrix.rs:126`，但二者**各自所在的 test binary 里只有 1 个测试**，无并发兄弟可竞争，故不构成 hazard（与原记录"同二进制 7 个测试"的情形不同）。升级条件：若任一二进制新增第二个测试 → 按同一 mutex / `ScopedXdgStateHome` 模式收口。
- **同类（已修）**: 进程全局状态竞态的另一实例是 CWD 而非 env —— `lattice-api` 四个测试文件裸调 `set_current_dir` 导致并发测试互相破坏，2026-07-27 由 **#109** 修复（删除多余的 CWD 修改 + 给生产 gix init 补恢复）。见 FIX.md 对应条目。
- **第三处（2026-07-27 fixed；#28 sweep 与 #111 各自独立撞到同一处，同一天分别修）**: `lancet-api` lib 二进制里 `config.rs` 的 `audit_state_root_prefers_absolute_xdg_state_home` 和 `audit.rs` 的 `IsolatedProjectAudit` 各持**自己**的 mutex 改同一个 `XDG_STATE_HOME`——两把锁串行不了任何东西。config 测试还原时 `remove_var`，恰好落在 audit 测试读路径的窗口里，于是 `tail_audit` 去真实 `~/.local/state/lattice/...` 找库并 panic；`XDG_TEST_LOCK` 随之 poison，`expect("xdg test lock")` 把 1 个失败放大成 9 个。修复：两处共用 `config::lock_xdg_state_home()`（单锁 + poison 恢复）。验证：`cargo test -p lancet-api --lib` 压测 25/25 绿；修复前同一门步骤在第二次 `cargo xtask ci` 复现失败。#28 与 #111 分支合并时收敛为 main 侧的单一实现（文档更完整），见 FIX.md 两条同日条目。

### L-21-01: `tests/msb_full_turn.rs` / `tests/pty_tui_smoke.rs` 真实 MSB 实跑 — `verification` / full-gate / owner-side
- **当前状态**: 2026-07-06 closed — owner KVM 实跑通过。`msb_plain_text_turn_commits_and_exit_zero` 在本机 KVM 完整门跑通：`turn_started → model_started → token_delta:"ok" → model_finished → turn_committed{status:"completed"}`，fake LLM 收到真 Chat Completions 请求，`session log --json` 交叉验证 commit_id 一致，exit 0。实跑过程中修了 6 个独立问题（`LATTICE_MSB_IMAGE` 未接线、`init_git_project` unborn HEAD、microsandbox `prebuilt` feature 关闭 + `MSB_HOME` 未注入、fake LLM base_url 用 `127.0.0.1` 被 net-guard 拒、`session log` 断言找错字符串），见 FIX.md `#21` 系列条目。L-21-02（探针探 audit 不探 MSB）作为独立 finding 保留；L-21-03 已于 2026-08-27 修复并移入 FIX.md。

### L-20: `SqliteSessionGraphStore` 单连接 `Mutex` 应用层串行化 — `performance` / `architecture`
- **当前状态**: 2026-06-27 closed — moot. The SQLite session graph store was retired
  (commit `687dd5c5 refactor(session): retire SQLite session store`); `crates/lattice-session/src/sqlite_graph/` no longer exists. The session graph is now git-backed (`GitSessionGraphStore` in `src/git_graph/`). The single-`Connection` `Mutex` concern does not apply to the git store.

### L-29-01: Phase 29 tmpfs root 路径以 child PID 命名 — `isolation-supervisor` / `future-proofing`
### L-29-02: Phase 29 setup report pipe 8192 字节读取上限 — `isolation-supervisor` / `diagnostics`
### L-29-03: FD cleanup 的 rlimit fallback 遇到 `close` 非 EBADF 错误会整体 fallback — `isolation-supervisor` / `robustness`
### L-29-04: Phase 29 live canary 和 fake matrix 覆盖充分但用途不同 — `verification` / `release-gate`
- **当前状态**: 2026-06-27 closed — moot. Landlock and netns command isolation were removed (commit `a29072e2 refactor(isolation): remove landlock and netns command isolation`); `crates/lancet-native/src/executor/supervisor/` no longer exists. The `[lancet.isolation]` profile section was also removed from `lattice-profile`. These Phase 29 supervisor findings referenced files (`supervisor/syscall.rs`, `supervisor/fd.rs`, `supervisor/linux.rs`, `supervisor/live_tests.rs`, `supervisor/env.rs`, `executor/shell.rs` setup-report pipe) that are gone. Full bodies preserved in git history.

### L-31-02: `LowerBaseline.regular_file_contents` 全量缓存项目文件字节 — `performance` / `memory`
- **当前状态**: 2026-06-27 closed — moot. Referenced `crates/lancet-native/src/executor/supervisor/snapshot.rs`, which was removed with the supervisor (commit `a29072e2`). The artifact overlay commit pipeline this guarded was part of the removed isolation system.

### L-34-01: SIGKILL/崩溃在 overlay teardown 之前会遗留孤儿 scratch，无启动清扫 — `robustness` / `hygiene`
- **当前状态**: 2026-06-27 closed — moot. Referenced `crates/lancet-native/src/executor/supervisor/overlay.rs` (`cleanup_overlay`, `OverlayPlan::deterministic`, `.lattice-overlays/`), which was removed with the supervisor (commit `a29072e2`). The overlay/scratch orphan scenario no longer applies to the production execution path (MSB/Upper workspace boundary in `lattice-workspace`).

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
- **已做**: 请求前通过 `net-guard::resolve_public_socket_addrs` 解析公网地址，`ClientBuilder::resolve_to_addrs` 钉住结果（`lattice-llm/src/client.rs`）。
- **已做**: request-time pinning 只绑定首跳，此前重定向可绕过它；#90 已给 provider 客户端设 `redirect::Policy::none()`，与 `fetch.web` 两个客户端一致，不再有第二跳。
- **残留**: `validate_base_url` 阶段的 DNS 仍为 best-effort（离线 CI 可能不解析）；权威防护在 request-time pinning，配置时校验不能单独当作闭环。

### M-05: `web_fetch` DNS rebinding / 黑名单 — `security`
- **已做**: 与 M-04 共享 request-time 钉 IP；已知 rebinding 后缀由 `net-guard` 的统一 denylist 硬拒绝，native `web.rs` 复用同一守卫。
- **残留**: 黑名单不可穷尽（新 wildcard→IP 服务、非列表域名）；需持续维护或改为启发式（主机名编码 IP 模式）+ 仍依赖钉 IP 作为主防线。

### M-TOOL-02: `lancet-runtime` uses synchronous wasmtime instead of async — `lancet-runtime` / #5 slice C
- **Location**: `crates/lancet-runtime/src/bindings.rs` (bindgen without `async`), `src/runtime.rs` (sync `instantiate`/`call_run`), `src/host.rs` (sync `Host` trait).
- **Problem**: The slice C plan §C.4–C.6 specifies async wasmtime: `bindgen!({ async: true })`, `instantiate_async`, `YieldCustom(1, tokio::task::yield_now())` in the epoch callback. The implementation uses synchronous wasmtime because wasmtime 46's `bindgen!` macro doesn't accept `async: true` as a key (the syntax may have changed in this version). Consequences: (1) the epoch callback uses `Continue(1)` instead of `YieldCustom` — long-running synchronous calls don't yield to the async runtime; (2) `Host` trait methods are sync (`fn cancelled() -> bool`) instead of async; (3) timeout/cancellation for long-running guests requires the epoch ticker to fire during synchronous execution, which works but doesn't yield.
- **Current status**: PARTIAL — all 26 tests pass; the noop component (microsecond execution) works correctly. Fuel exhaustion and survival are tested. The timeout path works for long-running calls but can't yield. `validate_tool_api_export` (§C.3 line 150) is not implemented — the gate relies on `Component::new` compilation + `instantiate` at call time to catch missing exports.
- **Plan**: Investigate wasmtime 46's async component-model API (may require `component-model-async` feature or different bindgen syntax). Switch to async wasmtime in a follow-up (slice D landed without changing the sync wasmtime choice). Add `validate_tool_api_export` to the gate. Use `YieldCustom` in the epoch callback once async is enabled.
