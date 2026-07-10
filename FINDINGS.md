# FINDINGS — Open Issues

未修复项，留给后续 alpha 主线。已修复项见 [FIX.md](./FIX.md)。

---

## HIGH

(None)

---

## MEDIUM

### ENV-OOM-01: 开发机常驻 Electron app 重度超售撞 24 并发 cargo 编译 → global OOM(已缓解,非代码 bug)— `environment` / dev-loop / #27
- **位置**: `xtask/src/ci.rs`(test step,已修);开发机环境(非 repo 代码)。完整根因账本在 `/tmp/lattice-oom-investigation/`(/tmp,不进 repo)。
- **问题**: `cargo test --workspace`(原 `xtask/src/ci.rs:44`)在 owner 的 24 核开发机上两次触发 **编译阶段** global OOM 致强制重启(2026-07-07 00:41:53 / 00:57:17)。00:41 OOM dump(348 个 uid 1000 进程)关键数:总 RSS 5.91 GB(物理 31 GB,未爆)、shmem 8.60 GB、swapents 8.42 GB(swap 14 GB 接近见底)、**total_vm 7478 GB(极度超售)**。编译工具链 RSS 合计仅 **2.41 GB**(rustc×24 + rust-lld×19 + cargo×1 + rust-analyzer×2),不是元凶。元凶是常驻 Electron app(electron×31 / obsidian×9 / cursor / codex / QQ / 微信 / wps / msedge ...)的 V8 巨型地址预留(total_vm 7478 GB)+ swap 见底。cargo 默认起 24 个 rustc(=核数),瞬时 codegen 分配是压垮骆驼的最后一根稻草。OOM killer 按 oom_score 杀 **adj=300 的 Electron**(不是 adj=200 的 rustc)→ 桌面崩溃 → 用户强制重启。`oom-kill:constraint=CONSTRAINT_NONE,global_oom`。**Codeberg #27 原文怀疑"某测试 spawn 大量子进程/proptest 爆炸"——日志否定:崩溃时无任何 test binary,纯编译阶段。**
- **当前状态**: MITIGATED(via `xtask ci` 改造,见 FIX.md `#27` 条目)。`xtask ci` test step 改为逐 crate(`cargo test -p <crate>`,cargo_metadata 拓扑排序)+ 每步硬超时(`LATTICE_TEST_TIMEOUT_SECS`,默认 300s)+ `process_group(0)` + `kill -KILL -<pgid>` 杀整树;`CARGO_BUILD_JOBS`(默认 4)cap 覆盖 fmt/clippy/build/test 全部门步骤。`LATTICE_SKIP_KNOWN_HANGS=1` 让门跳过 L-26-01 的 `fetch_effect_contract_negative` 挂起。验证:在本机(同 24 核)跑 `CARGO_BUILD_JOBS=4 cargo build --workspace --tests` PASS 14.89s(swap 0B);逐 crate 17/17 PASS 无失控;`LATTICE_SKIP_KNOWN_HANGS=1 ... xtask ci` 全 18 crate 绿,无 OOM 无孤儿。缓解不依赖关 app——jobs=4 cap 已把编译足迹压到常驻 app 能容忍的范围;但跑门前关掉非必要 Electron app 仍是更稳的实践(释放 total_vm 超售 + swap)。
- **升级条件**: 若 jobs=4 cap 在常驻 app 全开时仍 OOM(更重型编译单元、更大 codegen)→ 升为 HIGH,考虑降 jobs 到 2 或加 zram。
- **计划**: (a) 已落地 xtask ci 改造;(b) 推荐 owner 跑门前关非必要 Electron app(非强制);(c) CI(ubuntu/macos-latest,资源充足无桌面 app)不受影响,无需改 CI 配置。**#27 issue 应补 comment 修正方向**:真凶是环境侧超售,非 lattice 测试 bug,指向本 finding 与 FIX.md `#27` 条目。

### ENV-OOM-02: 并行测试暴露的 `unsafe env::set_var` 竞态 flake(非 OOM,被旧门编译期 OOM 掩盖)— `test-flakiness` / env-race
- **位置**: `crates/lattice-system-harness/src/layer_guard.rs:117-128`(`msb_requested_is_false_when_unset` / `msb_requested_is_true_when_one`,`unsafe { std::env::set_var/remove_var(MSB_ENV) }`);`crates/lattice-api/src/turn/cep_tests.rs:308`(`turn_runtime_constructs_one_adapter_per_turn`,panic at `test_support.rs:73` `build_production_turn_deps(...).expect("production deps")`,疑 XDG_STATE_HOME 路径竞态)。
- **问题**: 两个测试在并行执行(默认 `cargo test` thread count)时偶发 FAILED,但 `--test-threads=1` 下稳定通过。根因是 `std::env::set_var`/`remove_var` 是进程全局可变状态,Rust 2024 标记 unsafe 正是为此 data-race 风险——并行的另一个测试线程在 `remove_var` 与 `msb_requested()` 读之间 `set_var(MSB_ENV,"1")`,使 `msb_requested_is_false_when_unset` 的 `assert!(!msb_requested())` 偶发失败。`lattice-api` 的 flake 同源(CEP runtime 组合读 XDG_STATE_HOME,与并行测试写该 env 竞态)。**这些 flake 不是 #27 OOM 的元凶,也不是 `xtask ci` 改造引入的回归**——它们一直存在,只是旧 `cargo test --workspace` 门在编译阶段就 OOM,从未跑到测试执行,故从未暴露。逐 crate 隔离后这些 flake 才可见。
- **当前状态**: ACCEPTED RESIDUAL。`xtask ci` 跑测试用默认 thread count(与 `cargo test` 默认一致),偶发 flake 不阻塞门的正确性(重跑或 `--test-threads=1` 即绿)。两测试单线程下全绿已验证。
- **升级条件**: 若 flake 频率高到阻塞日常 `xtask ci` → 升为 MEDIUM-HIGH 并修(给 env-mutating 测试加 `#[serial]` 或抽离到独立进程),或给 `xtask ci` 加 `--test-threads=1` 选项。
- **计划**: (a) 短期接受 flake,文档化;(b) 中期可给 `msb_requested_*` 与 `turn_runtime_constructs_one_adapter_per_turn` 加 `serial_test::serial` 或把 env-mutating 断言改为 subprocess 隔离;(c) 不在 #27 范围内。

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

### T-23-01: #23 测试重构 4/6 - 109 条提升延期至 MSB/KVM 通道（偏差已记录）- `test-refactor` / #18 / #23
- **位置**: `docs/TEST-TRIAGE.md` §1.5；`docs/test-triage-decisions.json`（提升 140 条中的 109 条）
- **问题**: #23 按产品面分批执行 #22 triage。本机无 `/dev/kvm`、`LATTICE_HARNESS_MSB` 未开，无法落地或验证依赖真实完整 turn 的替代场景。两类提升被迫延期（**不删原内部测试**，否则普通 CI 即丢覆盖且无法验证替代）：
  (1) **39 条 MSB-bound**（`lattice-api` 36 + `lattice-agent` 3）-> `msb_full_turn` / `planned_observe_msb` / `planned_mutate_msb` / `planned_policy_msb` / `planned_retired_msb` / `planned_provider_fail_msb` / `planned_resume_msb` / `planned_secret_msb` / `planned_audit_ops_msb`。每条都需完整 turn，而完整 turn 默认走 Upper/Microsandbox。
  (2) **70 条 turn-bearing CLI**（`lattice-cli`）-> `session log` 带 committed turn、`audit query` 带 seeded 行、`session branch` 创建后、`run_integration_*`、`session resume`、`lancet cep_status` seeded 等；需经公开接口产生 committed turn 才能 black-box 覆盖，无 KVM 无法产生。
- **当前状态**: DEFERRED（#23 本批已执行 11/11 删除 + 31/140 提升 + 夹具收口 + 阻塞 bug 修复；`cargo xtask ci` 全绿）。判决口径未改；偏差已在 `docs/TEST-TRIAGE.md` §1.5 与 `FIX.md #23` 记录。#23 issue 保持 open（按用户要求不关闭、不 push）。
- **升级条件**: 若 5/6、6/6 或 KVM 可用环境落地 `planned_*_msb` 场景后仍有提升条目未删 -> 逐条核对并删，在本条勾销。
- **计划**: (a) KVM 可用环境落地 `planned_observe_msb` / `planned_mutate_msb` 等 MSB 场景 + 复用 `msb_full_turn` 产生 committed turn；(b) 按 `docs/TEST-TRIAGE.md` §1.5「延期」表逐条删除对应内部测试并更新 `docs/test-triage-decisions.json` 执行状态；(c) turn-bearing CLI 子集可在 MSB 通道补 `session log/show` seeded 场景后收口。

## LOW

### L-21-03: `net_guard::validate_base_url` 拒绝 `127.0.0.1`/`::1` IP 字面量但放行 `localhost` hostname — localhost 政策不对称 — `security` / net-guard / llm
- **位置**: `crates/net-guard/src/lib.rs:167-221`（`validate_base_url`，:194 scheme 检查列 `127.0.0.1`/`::1` 为 localhost，:206-213 IP 检查又把它们当 private/reserved 拒绝）；`crates/lattice-llm/src/client.rs:28-48`（`validate_and_read_key` 配置时调 `validate_base_url`）、`:342-344`（`is_localhost_request_host` 请求时放行 `127.0.0.1`/`::1`）、`:369-373`（`build_http_client_for_url` 请求时 localhost 跳过 `resolve_public_socket_addrs`）。
- **问题**: 两层 localhost 政策不对称，`127.0.0.1` 在同一函数内自相矛盾：
  - **函数内矛盾**: `validate_base_url` :194 把 `127.0.0.1`/`::1` 列为 localhost 放行 HTTP scheme，但 :206-213 的 IP 检查 `parse::<IpAddr>("127.0.0.1")` 成功 → `is_private_or_reserved` 返回 true（loopback）→ 拒绝。`:194` 说"这是 localhost"，`:207` 说"这是 private IP 不能连"——同一 host 两种待遇。`localhost` hostname 因 `parse::<IpAddr>` 失败而跳过 IP 检查，所以放行——不是被白名单，是 IP 检查只对 IP 字面量生效、hostname 绕过了它。
  - **配置时 vs 请求时矛盾**: `validate_base_url`（配置时，`client.rs:29`）拒绝 `127.0.0.1`；`build_http_client_for_url`（请求时，`client.rs:369`）的 `is_localhost_request_host` 放行 `127.0.0.1`。配置时校验先执行，`127.0.0.1` 在配置时就被拒，请求时的 localhost 例外根本到不了。
  - **后果**: harness fake LLM base_url 用 `http://127.0.0.1:port` 时产品报 `base_url targets private/reserved IP 127.0.0.1`，turn 在 LLM 调用前失败。harness 已改用 `http://localhost:port` 绕过（commit `dfb27ef0`），但根因是 net-guard 自身。
- **当前状态**: ACCEPTED RESIDUAL（harness 已绕开，产品侧 net-guard 不对称未修）。harness fake LLM base_url 用 `localhost` hostname 后 KVM 场景不再撞此问题。`resolve_public_socket_addrs`（:118-123）对 `localhost`/`127.0.0.1`/`::1` 一律拒绝（"cannot be resolved for remote pinning"），这是请求时 DNS pinning 路径——跟 `validate_base_url` 的配置时路径是不同的拒绝原因，但都体现"localhost IP 和 hostname 待遇不一致"。
- **升级条件**: 若产品需要支持 `http://127.0.0.1:port` 形式的 LLM base_url（如某些 provider SDK 硬编码 IP），或若 localhost 政策不一致导致用户配置困惑 → 升为 MEDIUM 并修 `validate_base_url` 的 IP 检查加 localhost 例外（:206-213 在 `is_private_or_reserved` 前先判 `is_localhost_host`）。
- **计划**: 修 `validate_base_url`：IP 检查前先判 `is_localhost_host(host)`，是 localhost 就跳过 `is_private_or_reserved`（与 :194 scheme 检查和 `client.rs:369` 请求时例外对齐）。非 #21 交付阻塞项——harness 已用 `localhost` hostname 绕开。

### L-26-01: `fetch_effect_contract_negative` 在 DNS 黑洞环境挂起；`is_private_or_reserved` 不覆盖 RFC 2544 段 — `robustness` / offline-ci / net-guard
- **位置**: `crates/lancet-native/src/effect_contract_tests/properties.rs:287-314`（`fetch_effect_contract_negative`）；`crates/net-guard/src/lib.rs:58-82`（`is_private_or_reserved`）；`crates/lancet-authority/src/policy/network.rs:106-204`（`validate_fetch_url`）；`crates/lancet-native/src/handlers/external.rs:44-74`（`handle_fetch_as` → `fetch_url`）。
- **问题**: `fetch_effect_contract_negative` 是 `lancet-native` lib 测试套件的一部分（非 `#[ignore]`、无 offline-gate、无 env 跳过），dispatch `Fetch` 到 `https://evil.example.com/steal`。`NetworkPolicyConfig::safe_defaults()`（`allow_public_https=true`、`allowed_hosts=[]`）放行非 localhost https；`validate_fetch_url` 对主机名（非 IP 字面量）不解析 DNS，直接 Ok。`fetch_url` 走 reqwest（`handlers/external.rs:28` 有 30s `timeout`）。在 DNS 把 `evil.example.com` 解析到 `198.18.0.63`（RFC 2544 benchmarking 段 198.18.0.0/15）且该 IP 被**网络黑洞路由**的环境下：`net-guard::is_private_or_reserved` 不覆盖 198.18.0.0/15（仅查 loopback/RFC1918/link-local/doc/10/172.16-31/192.168/169.254/127/0），IP 被判为公网放行，`fetch_url` 对 `198.18.0.63:443` 发 HTTPS，SYN 无响应，reqwest 30s timeout 在本沙箱观察窗口内未生效 → 测试挂起（>60s，libtest 报 "has been running for over 60 seconds"），阻塞 `cargo test -p lancet-native` 与依赖它的 `cargo xtask ci` / `release-gate-v1.3.sh` / `release-gate-v1.4.sh` 的 step 3。
- **当前状态**: ACCEPTED RESIDUAL（环境鲁棒性 / 离线 CI 友好性）。`fetch_effect_contract_negative` 在 `88e23c3d`（远早于 #26）引入；#26 xtask 迁移系列（8 个 commit）对 `lancet-native`/`lancet-authority`/`net-guard` **零文件触碰**，`git diff bdf8204c~1 HEAD` 显示相关源码逐字未改 → 同一挂起在迁移前后以完全相同方式发生，非迁移回归。正常 CI（ubuntu/macos-latest，`evil.example.com` 快速 NXDOMAIN 或 reqwest 30s timeout 生效）该测试通过。本沙箱实测：`lancet-native --lib` 挂起前 93 个测试全 `ok`、0 failed；其余 crate（xtask 24、lattice-cli 185、release_smoke 18、`--workspace --exclude lancet-native` 多套件）全绿。
- **升级条件**: 若离线/air-gapped CI 成为常态且需跑 `lancet-native` 全套件 → 升为 MEDIUM；或若 `is_private_or_reserved` 被要求覆盖更多保留段（RFC 2544 198.18.0.0/15、RFC 5737 文档 IP 203.0.113.0/24 等）作为 SSRF 防御纵深 → 升为 MEDIUM 并扩 `is_private_or_reserved`。
- **计划**: （a）短期不改测试行为，接受正常 CI 下通过；（b）可选加固 `is_private_or_reserved` 覆盖 198.18.0.0/15（与现有 169.254/127/0 段同列拒绝），作为 SSRF 纵深而非本测试的主修复；（c）若要彻底消除离线环境对 `lancet-native` 的阻断，给 `fetch_effect_contract_negative` 加 `#[ignore]` 或 env-gated 跳过（如 `LATTICE_SKIP_NETWORK_TESTS=1`），但需权衡是否弱化正常环境下对该契约的覆盖。本 finding 不阻塞 #26 迁移收尾。

### L-21-02: `require_msb_env` 探针探的是 audit 路径，不验证 MSB stack 可用 — `verification` / harness / full-gate
- **位置**: `crates/lattice-system-harness/src/layer_guard.rs`（`require_msb_env`，:54-106）；`crates/lattice-cli/src/commands/lancet.rs`（`run_status` → `collect_status`）；`crates/lancet-api/src/status.rs`（`collect_status`，:31-40）。
- **问题**: `require_msb_env` 用 `lattice lancet status` 作 MSB stack 探针，但 `lancet status` 只读 project audit SQLite store（`collect_status` → `CepAuditStore::open` + `query_rows`），**完全不碰 microsandbox daemon、`msb` binary、libkrunfw 或 KVM**。audit DB 不存在时直接返回空 `Ok`（`status.rs:35-40`）。因此探针 exit 0 只证明"产品 CLI 能跑 + audit 路径可读"，不证明"MSB stack 能起 Upper"。owner 实跑时 `msb` binary 缺失（未装 / `MSB_HOME` 指向隔离 HOME 导致 SDK 找不到），探针照样过，turn 才在 Upper 创建阶段撞 `msb binary not found`——失败信号比应有的晚了一整层。
- **当前状态**: ACCEPTED RESIDUAL（harness 验证缺口）。KVM 场景已通过 `MSB_HOME` env 注入 + `prebuilt` feature 修复了 `msb` 可达性（见 FIX.md `#21: enable microsandbox prebuilt feature` 条目），但探针本身仍探错对象——它该探的是"能起 Upper"（如尝试创建一个短命 sandbox 并立刻销毁），不是"能读 audit"。
- **升级条件**: 若 KVM 完整门成为 CI 常态且探针误绿导致延迟失败成为痛点 → 升为 MEDIUM 并重写探针。
- **计划**: 将 `require_msb_env` 探针改为真正验证 MSB stack（如 `lattice` 跑一个最小 Upper smoke，或直接调 microsandbox SDK 创建+销毁一个空 sandbox），而非读 audit。非 #21 交付阻塞项；当前 `msb binary not found` 已由 `MSB_HOME` 注入 + `prebuilt` feature 兜住，探针误绿的窗口收窄到"MSB_HOME 对了但 KVM/libkrunfw 坏了"这一窄场景。

### L-11-01: `tail_audit` 批次循环有 `yield_now` 但无专用 `CancellationToken` + CLI 接线 — `robustness` / operator-path
- **位置**: `crates/lancet-api/src/audit.rs`（`tail_audit` 批次循环,`tokio::task::yield_now().await` 在 L235;循环体 `query_rows` 同步）
- **问题**: 批次循环底部已加 `tokio::task::yield_now().await`,drop-future 取消可在批次边界生效。但无专用 `CancellationToken` 从 CLI 接入 `tail_audit`,`lattice audit --tail` 的 Ctrl-C 仅靠默认 future-drop,无显式信号接线。退役前的 native handler 经 `HandlerContext { cancellation: Cancellation }` 有 per-batch watch 检查;direct-store 路径用 `yield_now` 兜底但未做专用 token + CLI 接线。
- **当前状态**: ACCEPTED RESIDUAL(downgraded from LOW-MEDIUM → LOW)。已做:`yield_now().await` 批次间 yield(取消可在批次边界生效)。未做(最优处理):专用 `CancellationToken` + `lattice audit --tail` Ctrl-C 信号接线。最坏工作量由 `AUDIT_TAIL_MAX_BATCHES × AUDIT_TAIL_MAX_ROWS`(1000 × 1000)硬上限兜底;超限返回 `LancetError::Config` 并带 `last_audit_id` 供 `--since` resume。`--tail` 是操作员路径,典型 store 量级毫秒级完成。`tail_audit` doc comment 已显式写明该降级契约。
- **升级条件**: 若 audit store 量级进入百万行常态、或 `--tail` 被接入长驻 UI/streaming 消费方需要即时取消响应 → 升为 MEDIUM 并加 `CancellationToken` + CLI 信号接线。
- **计划**: 暂不实现专用 token + CLI 接线;维持 `yield_now` + 批次上限兜底 + 显式契约文档。

### L-11-02: 活跃 turn 并发读取仅由合成 writer 测试覆盖,无 live-turn 集成测试 — `verification` / concurrency-invariant
- **位置**: `crates/lancet-api/src/audit.rs`(`operator_audit_reads_concurrent_with_active_writer`,约 514–627 行);`crates/lancet-audit/src/audit.rs`(`query_rows` SQL 谓词 + `init_schema` WAL)
- **问题**: 并发读不变量(读不阻塞 writer、只返回 finalized 行、单次读 snapshot 一致)由结构性保证(SQL `ended_at IS NOT NULL AND result_or_error IS NOT NULL` + WAL 语句级 snapshot)+ `operator_audit_reads_concurrent_with_active_writer` 回归测试覆盖。但该测试的 writer 是**合成**的 —— 直接调 `CepAuditStore::start_call`/`finalize_call`,不 spawn 活跃 `LancetRuntime`/`stream_turn`。无 live-turn 集成测试验证"真 turn 在另一 task 写 audit store 的同时从并发 task 调 `query_audit`/`tail_audit`"的端到端并发读行为。
- **当前状态**: ACCEPTED RESIDUAL(downgraded from MEDIUM → LOW)。合成 writer 测试在 store 并发边界(SQLite WAL + finalize 谓词)上与 live turn 等价 —— live turn 最终也走同一 `CepAuditStore` API,无额外并发 hazard —— 故接受合成 writer 测试为充分兜底(downgrade),不补 live-turn 集成测试(最优处理)。`audit.rs` module doc 已显式写明该降级契约。
- **升级条件**: 若 audit 写路径引入 `CepAuditStore` 之外的并发 hazard(如跨进程写、外部锁、非 WAL journal mode)、或 `query_rows` 改为跨多次 `SELECT` 拼接而中间释放 snapshot → 升为 MEDIUM 并补 live-turn (`LancetRuntime`/`stream_turn`) 并发读集成测试。
- **计划**: 暂不补 live-turn 集成测试;维持合成 writer 测试 + 结构保证 + 显式契约文档。

### L-34-02: Direct-mode `changed_paths()` porcelain enforcement weaker than Envelope snapshot diff — `security` / `defense-in-depth`
- **位置**: `crates/lattice-api/src/turn/workspace_backend.rs:321`（`changed_paths` → 委托 `Backend::git_status`，不再内联 porcelain）；生产 porcelain 在 `crates/lattice-workspace/src/microsandbox.rs:1050`（Upper 跑 `git status --porcelain=v1 -z`），解析器 `crates/lattice-workspace/src/git_status.rs`；WATCH 锚点 `crates/lattice-workspace/AGENTS.md:49-51`。
- **问题**: Direct-mode post-execution write-scope enforcement uses `git status --porcelain=v1`, which omits all gitignored paths. Envelope-mode uses filesystem snapshot diff (`capture_workspace_snapshot` / `compute_changed_paths`) that only skips `target/`, `.lattice-overlays/`, `.git/`. A Direct workspace-mutating command can write to gitignored paths outside `allowed_write_paths` (e.g. `.env`, `build/`) and pass the gate because porcelain omits ignored paths. Additionally, a TOCTOU exists: a background process spawned by the command can write after `exec` returns but before `changed_paths()` runs.
- **当前状态**: ACCEPTED RESIDUAL. The Upper workspace is per-turn (created in `build_production_turn_deps`, destroyed when `ProductionTurnDeps` dropped). Session graph, audit trail, and credential store all live in the Lower (host) and cannot be affected by Upper writes. The LLM already has `RunCommand` capability, so writing to gitignored paths within a turn doesn't expand its attack surface. The porcelain gap weakens the `allowed_write_paths` defense-in-depth policy *within* the Upper, but creates no host-impact path.
- **范围变更 (2026-06-27)**: Direct-mode naming was retired (commits `c160d3eb`, `e939ae02`, `dc507d15`); the production execution path is now the MSB/Upper workspace boundary. The underlying changed-path porcelain gap still applies to Upper→Lower write-back detection and is retained as a WATCH in `crates/lattice-workspace/AGENTS.md`. The escalation conditions below still hold.
- **范围变更 (2026-06-30)**: issue #4（commit `691c0b67`）永久退役 CEP command 执行，`crates/lancet-native/src/handlers/command.rs` 整文件删除——原 post-exec command scope check 执行点已不存在，位置引用相应移除。本 finding 残留 locus 收敛为 porcelain 化的 `changed_paths()`（Upper→Lower write-back 检测）；退役仅移除消费方，porcelain 不列 gitignored 路径的底层缺口不变，故维持 ACCEPTED RESIDUAL。
- **升级条件**: If the Upper ever becomes per-session (gitignored writes persist across turns) → escalate to REQUIRES FIX. If Upper→Lower write-back is added (e.g., artifact commit) → escalate. If Upper gains network egress → TOCTOU becomes exploitable for exfiltration.
- **计划**: Optional future work: switch `changed_paths()` to filesystem snapshot diff for parity with Envelope mode. Do NOT use `--porcelain --ignored` (false positives on `target/` build artifacts).

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

### L-21-01: `tests/msb_full_turn.rs` / `tests/pty_tui_smoke.rs` 真实 MSB 实跑 — `verification` / full-gate / owner-side
- **当前状态**: 2026-07-06 closed — owner KVM 实跑通过。`msb_plain_text_turn_commits_and_exit_zero` 在本机 KVM 完整门跑通：`turn_started → model_started → token_delta:"ok" → model_finished → turn_committed{status:"completed"}`，fake LLM 收到真 Chat Completions 请求，`session log --json` 交叉验证 commit_id 一致，exit 0。实跑过程中修了 6 个独立问题（`LATTICE_MSB_IMAGE` 未接线、`init_git_project` unborn HEAD、microsandbox `prebuilt` feature 关闭 + `MSB_HOME` 未注入、fake LLM base_url 用 `127.0.0.1` 被 net-guard 拒、`session log` 断言找错字符串），见 FIX.md `#21` 系列条目。L-21-02（探针探 audit 不探 MSB）和 L-21-03（net-guard localhost 政策不对称）作为独立 finding 保留。

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
- **残留**: `validate_base_url` 阶段的 DNS 仍为 best-effort（离线 CI 可能不解析）；权威防护在 request-time pinning，配置时校验不能单独当作闭环。

### M-05: `web_fetch` DNS rebinding / 黑名单 — `security`
- **已做**: 与 M-04 共享 request-time 钉 IP；已知 rebinding 后缀由 `net-guard` 的统一 denylist 硬拒绝，native `web.rs` 复用同一守卫。
- **残留**: 黑名单不可穷尽（新 wildcard→IP 服务、非列表域名）；需持续维护或改为启发式（主机名编码 IP 模式）+ 仍依赖钉 IP 作为主防线。
