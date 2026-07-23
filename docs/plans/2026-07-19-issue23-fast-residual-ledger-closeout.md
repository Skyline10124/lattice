# #23 实现计划：快速测试残留与分类账收口（测试重构 4/6）

- 状态：已完成（2026-07-19；ledger/prose 已落地，见 FIX.md #23）
- 票：<https://github.com/Skyline10124/lattice/issues/45>（原 Codeberg #23）
- 上级：#18（PRD）/ #19（RFC，已接受）。判决书 = `docs/TEST-TRIAGE.md` + `docs/test-triage-decisions.json`（1449 条，身份不变）。
- 排序依据：roadmap #16 既定顺序，#23 先于 #5 开工。
- 独立切片计划（均含示例代码）：
  [1：fast lancet CLI](2026-07-19-issue23-slice1-fast-lancet-cli.md) →
  [2：branch fast scenarios](2026-07-19-issue23-slice2-branch-fast-scenarios.md) →
  [3：退役 KVM-lane 词汇/映射](2026-07-19-issue23-slice3-retired-lane-vocabulary.md) →
  [4：prose/ledger 一致性](2026-07-19-issue23-slice4-ledger-prose-consistency.md)。
  切片 3/4 文档独立，但当时必须同一 PR 相邻 commit（账本原子性）。

## 0. 收口前锚点（已核验 2026-07-19；现已清零）

- ledger 当时 1449 条：keep 1338 / elevate 96 / delete 15（15 条删除已全部执行）。
- elevate 96 = **41 条 fast_binary + 55 条退役 KVM-lane replacement**；另有 2 条 keep 仍挂退役
  KVM-lane `target_layer`，所以待改 layer 总数曾为 57。切片 3 后该类 layer 已清零。
- 41 条 fast 中 **36 条已接管完成**；残留 5 条当时全在 `lattice-cli`：
  1. `cep_status_missing_db_returns_empty_state`（`lancet.rs:289`）→ `planned_lancet_cli`，场景未建。
  2. `project_branch_creation_json_output_includes_branch`（`session.rs:1284`）→ 部分覆盖，缺 branch 创建 JSON 场景。
  3. `project_branch_persists_through_reopen`（`session.rs:1345`）→ 部分覆盖，缺 reopen 持久化场景。
  4. `smoke_session_status_list_shows_seeded_session`（`release_smoke.rs:185`）→ 已完整接管，冗余待删。
  5. `smoke_session_status_specific_session`（`release_smoke.rs:210`）→ 已完整接管，冗余待删。
- #40 已删 Upper 与退役 KVM-lane harness 门禁通道：ledger 里带退役 lane 后缀的 replacement 名与
  退役 KVM-lane `target_layer` 全部成为**死词汇**（TEST-TRIAGE §1.5 已声明延期理由失效）。
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
| `session.rs:1284` `project_branch_creation_json_output_includes_branch` | 新场景 1 接管 |
| `session.rs:1345` `project_branch_persists_through_reopen` | 新场景 2 接管 |
| `release_smoke.rs:185` `smoke_session_status_list_shows_seeded_session` | prose §1.5 已判「完整接管待删」（`fast_session_new_creates_session_and_appears_in_list` 等） |
| `release_smoke.rs:210` `smoke_session_status_specific_session` | 同上（`fast_session_status_specific` 系列） |

夹具收口（票文「仅由测试重复造成的 fixtures」）：删除后检查 `release_smoke.rs` 的
`seed_session` 类 helper 是否成为孤儿，孤儿即删；**不为已删 Upper / 退役 KVM-lane API 新增 shared helper**。

## 3. 切片 3：退役 KVM-lane 词汇 + #24 精确映射

### 3.1 精确处置（不做大类正则替换）

当时 57 条 stale layer 逐一分成：

- 1 条历史 harness identity 已由 #40 plain-text scenario 接管；
- 44 条映射到 **44 个 assertion-level #24 scenario ID**（例如 basic event contract、existing-session
  RunTurnResult、message assembly、audit query/tail/replay、五种 session graph isolation 分别命名），
  禁止 `planned_mutate_direct` 一类 family fallback；
- 11 条绑定已删除 Upper/promotion/prune 产品面的历史测试补记为 delete，不向 #24 虚构场景；
- 1 条现存 PTY smoke 只把 layer 改为 `direct_local_binary`，decision=keep。

精确 57 条 identity、44 个 scenario ID 与预期 decision delta 见
[切片 3 独立计划](2026-07-19-issue23-slice3-retired-lane-vocabulary.md)。

### 3.2 范围红线

- 不实现任何 Direct-Local runtime 行为（#24 范围）。
- 语义核未被已落地场景形成 assertion superset 的内部测试一律保留。
- `crate/file/line/name/async/ignored` identity 字段不变；decision 只允许独立计划列出的显式 delta。

## 4. 切片 4：prose ↔ ledger 一致性收口

- 更新 §1.5 执行状态：fast residual 归零表、退役 KVM-lane 词汇退役记录、删除计数。
- 各 crate 小节头计数与 JSON 重算对齐；按当时基线预期 keep 1337 / elevate 86 / delete 26；
  总数维持 1449，identity 不变。
- 一致性核验脚本化跑一次（python 重算 JSON 分布 vs prose 计数）；结果贴 PR，不新增常驻工具。

## 5. 验收对照（票文逐条）

| 票文验收 | 对应切片 |
| fast residual 为零；`lancet status` + branch create/reopen 有 external-binary 覆盖 | 切片 1 + 2 |
| 已接管的内部测试删除；未接管的不提前删 | 切片 1 + 2 + 切片 3 assertion-superset 表 |
| 退役 KVM-lane replacement 全部分成「本票完成」或「#24 精确接管」，无含糊兜底 | 切片 3 |
| prose 与 ledger 计数/layer/replacement/状态一致 | 切片 4 |
| harness 只依赖 `LATTICE_BIN` 与公开接口 | 切片 1 + 2 不加产品 crate 依赖（`cargo xtask harness-boundary` 门） |
| fmt、clippy、workspace tests、fast harness 通过 | 下方门禁 |

门禁：

```
cargo xtask ci                        # fmt / clippy -D warnings / build / harness-boundary / workspace tests
LATTICE_BIN="$(pwd)/target/debug/lattice" cargo test -p lattice-system-harness --test fast_lancet_cli --test fast_session_cli
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
- 57 条 stale layer 中含 42 个 exact planned ID + 12 条显式 decision delta；只允许显式 dict/set
  批改并用 identity snapshot 断言，禁止正则 family fallback。
