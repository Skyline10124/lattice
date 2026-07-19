# #23 实现计划：快速测试残留与分类账收口（测试重构 4/6）

- 票：<https://codeberg.org/AstRin/LATTICE/issues/23>（`ready-for-agent`，无前置依赖）
- 上级：#18（PRD）/ #19（RFC，已接受）。判决书 = `docs/TEST-TRIAGE.md` + `docs/test-triage-decisions.json`（1449 条，身份不变）。
- 排序依据：roadmap #16 既定顺序，#23 先于 #5 开工。

## 0. 现状锚点（已核验 2026-07-19）

- ledger 1449 条：keep 1338 / elevate 96 / delete 15（15 条删除已全部执行）。
- elevate 96 = **41 条 fast_binary + 55 条 msb-bound**（54 条 `real_microsandbox` 延期 + 1 条
  `run_integration_commits_existing_session`）。
- 41 条 fast 中 **36 条已接管完成**；残留 5 条，全在 `lattice-cli`：
  1. `cep_status_missing_db_returns_empty_state`（`lancet.rs:289`）→ `planned_lancet_cli`，场景未建。
  2. `project_branch_creation_json_output_includes_branch`（`session.rs:1284`）→ 部分覆盖，缺 branch 创建 JSON 场景。
  3. `project_branch_persists_through_reopen`（`session.rs:1345`）→ 部分覆盖，缺 reopen 持久化场景。
  4. `smoke_session_status_list_shows_seeded_session`（`release_smoke.rs:185`）→ 已完整接管，冗余待删。
  5. `smoke_session_status_specific_session`（`release_smoke.rs:210`）→ 已完整接管，冗余待删。
- #40 已删 Upper/MSB/`LATTICE_HARNESS_MSB`：ledger 里 `*_msb` replacement 名与
  `real_microsandbox` target_layer 全部成为**死词汇**（TEST-TRIAGE §1.5 "#40 状态更新"已声明延期理由失效）。
- #40 落地 `tests/direct_local_turn.rs` 骨架（ordinary lane，非 ignore）：plain-text turn commit、
  session reopen、mutate policy denial 三场景已通过。
- harness 契约：仅 `LATTICE_BIN` + 公开接口（`driver.rs:14`，AGENTS.md）；无产品 crate 依赖。

## 1. 切片 1：`fast_lancet_cli` 场景 + 删内部测试

新文件 `crates/lattice-system-harness/tests/fast_lancet_cli.rs`：

- `fast_lancet_status_missing_audit_store_reports_empty_state`：project 无 audit DB 时
  `lattice lancet status --json` → exit 0，`recent_calls` 空、verdict 为 null（对齐内部测试断言核心）。
- `fast_lancet_status_text_mode_empty_state`：text 模式同态冒烟（顺手，同一夹具）。

接管后删除 `lattice-cli/src/commands/lancet.rs:289` 的内部测试。
ledger：该条 reason 改「已删除原内部测试，由已落地 fast_lancet_cli 接管」；decision 维持 elevate。

## 2. 切片 2：project branch fast 场景 + 删 4 条内部/smoke 测试

扩展 `crates/lattice-system-harness/tests/fast_session_cli.rs`（现有 `create_session` 夹具复用）：

- `fast_session_new_json_includes_project_branch`：`session new --project-branch <b> --json`
  输出含 branch 字段（接管 `session.rs:1284`）。
- `fast_session_branch_persists_through_reopen`：create → 独立第二次 CLI 调用
  `session status/branch --json` 断言 branch 仍在（接管 `session.rs:1345`；进程边界即 reopen 语义，
  比原内部测试更接近产品行为）。

两场景通过后删除：

| 删除 | 依据 |
| --- | --- |
| `session.rs:1284` `project_branch_creation_json_output_includes_branch` | 新场景 1 接管 |
| `session.rs:1345` `project_branch_persists_through_reopen` | 新场景 2 接管 |
| `release_smoke.rs:185` `smoke_session_status_list_shows_seeded_session` | prose §1.5 已判「完整接管待删」（`fast_session_new_creates_session_and_appears_in_list` 等） |
| `release_smoke.rs:210` `smoke_session_status_specific_session` | 同上（`fast_session_status_specific` 系列） |

夹具收口（票文「仅由测试重复造成的 fixtures」）：删除后检查 `release_smoke.rs` 的
`seed_session` 类 helper 是否成为孤儿，孤儿即删；**不为旧 Upper/MSB API 新增 shared helper**。

## 3. 切片 3：`*_msb` 词汇退役 + #24 精确映射

### 3.1 词汇替换（ledger + prose 同步）

| 旧 | 新 |
| --- | --- |
| target_layer `real_microsandbox` | `direct_local_binary` |
| `msb_full_turn` | `direct_local_turn`（已落地骨架场景按名引用） |
| `planned_observe_msb` 等 `planned_*_msb` | `planned_*_direct`（scope 后缀统一替换） |

### 3.2 逐条映射（54 条延期 + 1 条冗余）

- **已被 #40 骨架接管的条目就地收口**：`run_integration_commits_existing_session`
  （`run_tests.rs:145`，replacement 原 `msb_full_turn` 已落地为
  `direct_local_plain_text_turn_commits_and_exit_zero`，ordinary lane 通过）→ 删除内部测试，
  reason 改「已删除，由 direct_local_turn 接管」。其余条目若语义核已被骨架三场景
  （plain commit / reopen / policy denial）完整覆盖，同样就地收口；语义核不符的**不删**。
- **其余条目**逐条改 replacement 为 #24 矩阵中的**具名场景**
  （`planned_observe_direct` / `planned_mutate_direct` / `planned_retired_direct` /
  `planned_provider_fail_direct` / `planned_secret_direct` / `planned_audit_ops_direct` /
  `planned_session_graph_direct` / `planned_session_history_direct`），
  多场景复合条目保留 `|` 复合表示；禁止兜底三元泛化（review-fix 六轮教训）。
- TEST-TRIAGE §1「场景目录」补登新场景 ID 及其 #24 归属；§1.5 延期账本表改写为
  「#24 接管清单」，逐条可审计。

### 3.3 范围红线

- 不实现任何 Direct-Local runtime 行为（#24 范围）。
- 语义核未被已落地场景覆盖的内部测试一律保留——即使词汇改名。

## 4. 切片 4：prose ↔ ledger 一致性收口

- 更新 §1.5 执行状态：fast residual 归零表、`*_msb` 退役记录、删除计数。
- 各 crate 小节头计数（如 `lattice-cli`（保留 118 / 提升 57 / 删除 10））与 JSON 重算对齐；
  总数维持 1449，身份不变。
- 一致性核验脚本化跑一次（python 重算 JSON 分布 vs prose 计数）；结果贴 PR，不新增常驻工具。

## 5. 验收对照（票文逐条）

| 票文验收 | 对应切片 |
| --- | --- |
| fast residual 为零；`lancet status` + branch create/reopen 有 external-binary 覆盖 | 切片 1 + 2 |
| 已接管的内部测试删除；未接管的不提前删 | 切片 1 + 2 + 3.2（红线 3.3） |
| `*_msb` 全部分成「本票完成」或「#24 精确接管」，无含糊兜底 | 切片 3 |
| prose 与 ledger 计数/layer/replacement/状态一致 | 切片 4 |
| harness 只依赖 `LATTICE_BIN` 与公开接口 | 切片 1 + 2 不加产品 crate 依赖（`cargo xtask harness-boundary` 门） |
| fmt、clippy、workspace tests、fast harness 通过 | 下方门禁 |

门禁：

```
cargo xtask ci                        # fmt / clippy -D warnings / build / harness-boundary / workspace tests
LATTICE_BIN=target/debug/lattice cargo test -p lattice-system-harness --test fast_lancet_cli --test fast_session_cli
cargo xtask lancet-boundary
```

## 6. PR 切片顺序

1（lancet fast + 删 1）→ 2（branch fast + 删 4 + 夹具孤儿清理）→ 3（词汇退役 + 映射 + 就地收口）→ 4（prose/ledger 同步，随 3 同 commit 或紧随）。
1 与 2 可并作一个 PR；3+4 必须同 PR（账本原子性）。

## 7. 残余风险

- 切片 3 就地收口的「语义核已覆盖」判定是本票唯一判断题：逐条在 PR 描述列
  骨架场景 ↔ 内部测试断言的对照，供复审否决。
- `session new` 的 `--project-branch` 为必填（`session.rs:47-49`）：branch 创建场景实为
  session 创建路径的断言强化，若 CLI 面与预期不符（如 branch 字段在 status 而非 new 输出），
  以实际公开 JSON 契约为准调整断言，不改产品面。
- 55 条映射是机械量大但无技术风险的工作；用脚本批改 + 人工抽查，避免手滑改动测试身份字段。
