# uninode / CEP / context-compiler 适配度分析

> **归档快照。** 本文依赖的 v1.5 `.planning/` 状态和 CEP 方法集已经过时，
> 仅保留为未落地 Uninode 方案的历史分析，不代表当前架构或实施计划。

Status: DRAFT
Date: 2026-06-19
Depends on:
- [uninode-spec.md](./uninode-spec.md)
- [context-compiler.md](./context-compiler.md)
- [uninode-prior-art.md](./uninode-prior-art.md)

> 评估对象：一套**已设计、未落代码**的执行模型（uninode 数据模型 + CEP turn lifecycle +
> context compiler）。评估目标：它与 (1) 正在实施的 **v1.5-alpha**、(2) 整个 LATTICE
> 现行架构 的适配度，以及主对话推荐的**最小端到端切片 (a)**
> （`base bundle(File/Operation) + fanotify 建图 + 无学习 Seed-Expand-Rank-Pack`）能否落地。
>
> 证据标注：**[已实施]** = 仓库现有代码；**[计划中]** = v1.5 ROADMAP/REQUIREMENTS 已排期未写；
> **[仅设计]** = 只在 uninode 三文档里，无代码无排期。

---

## 0. TL;DR

- **uninode 的"控制面/数据面两层 + Upper/Lower + host-owned promote + 保留 CEP 边界"
  与 v1.5-alpha 的架构高度同构**——不是巧合：`docs/archive/uninode/lattice-cep-sequence.mmd` 用的就是
  v1.5 设计契约（`docs/archive/designs/2026-06-13-cube-upper-git-session-design.md`）的
  Upper/Lower 术语。uninode 是**把 v1.5 的执行骨架，叠加一层"图 + context 编译"语义**。
- **但二者不是同一层的东西。** v1.5 = 执行边界 / promotion / Git session（Phase 33–39）；
  uninode 的**增量**（`.uninode/` 图、fanotify 建图、Seed-Expand-Rank-Pack 编译器、学习边权）
  **全部是 [仅设计]，且不在 v1.5 任何 phase 的范围内**。
- **三处硬冲突**：(1) uninode 数据面要**事件流(fanotify)**，现 `Workspace` trait 只有
  请求/响应 `exec`，无流；(2) uninode 的 "CEP" 仍引用 `session.envelope` / 实时 promote
  validate，而 v1.5 **Phase 37 正要删掉 envelope**、promote 改为 **Git baseline CAS**；
  (3) uninode 把图当 source-of-truth 的潜在倾向，与 v1.5 "project repo = source of truth,
  图可重建" 必须对齐（spec 已自洽地选了后者，需保持）。
- **(a) 切片可行，但应等 Phase 34，并明确定位为"v1.5 之后/之外的探针"**，独立成
  `lattice-uninode` 新 crate，**只读旁路**，不碰 promote / envelope / session。它最大的
  价值（试探 fanotify 可靠性）**依赖 Phase 34 把 exec 真正搬进 Upper**——在那之前
  sandbox 内没有 agent 触发的文件事件可抓。

---

## 1. v1.5-alpha 是什么（从仓库挖出的现状）

**来源**：`.planning/ROADMAP.md`、`.planning/STATE.md`、`.planning/PROJECT.md`、
`.planning/REQUIREMENTS.md`、`docs/archive/designs/2026-06-13-cube-upper-git-session-design.md`、
`AGENTS.md`、`FINDINGS.md`、`git log`/`git tag`。

**名称**：v1.5-alpha **Upper Workspace & Git Session Backend**，Phases 33–39
（`.planning/ROADMAP.md:9,22-34`）。最新 tag `v1.4-alpha`；v1.5 **未发布**，进行中。
当前分支 `docs/uninode-context-compiler`。

**目标**（`.planning/ROADMAP.md:24`、`PROJECT.md:21-34`）：把 v1.4 的"宿主 overlay
命令隔离"换成 **microVM Upper 工作区**——模型在 Upper 里自由执行，结果经 **Git
promotion** 提升到干净的 **Lower** worktree；并把 session graph 从 **SQLite 迁到 Git**。

**进度**（`.planning/STATE.md:9-14`、`ROADMAP.md:154-162`）：
- **Phase 33 Workspace Adapter Production Hardening — 已完成并验证**（6/6 plan，2026-06-14，
  KVM 实跑通过）。交付 `crates/lattice-workspace`：`Workspace` trait + `MicrosandboxWorkspace`
  (libkrun) + `DeterministicWorkspace` (CI)。WS-01..04 / EGRESS-01 / CRED-01 全部 Complete
  （`REQUIREMENTS.md:14-17,47-48,89-94`）。
- **Phase 34–39 — 全部 Not started**（`ROADMAP.md:157-162`，0/TBD plans）：
  - **34 CEP Execution Routing Through Upper**：把 `exec.command_run` 与文件 observe/mutate
    路由进 Upper，模型永不碰 Lower（EXEC-01/02/03）。
  - **35 Promotion Module and Lower Worktree**：host-owned promotion，candidate 校验、
    baseline compare-and-swap、journaled 可恢复 Lower 物化、verify 绑定 `candidate_oid`、
    project+session ref 原子事务（PROMO-01..07, LOWER-01）。
  - **36 Turn Lifecycle and Disposition**：双字段终态（execution status + workspace
    disposition）、provisional-dirty 回灌 agent cleanup、从 microVM snapshot resume（LIFE-01..03）。
  - **37 Overlay and Envelope Removal**：**删宿主 overlay 引擎**、**删 `session.envelope`
    概念与 wire types / 可变 authority state**、catalog 收到 **9 个方法**，保留 HashLine /
    `mutate.edit`（RM-01/02/03）。
  - **38 Git Session Backend**：`GitSessionGraphStore` 取代 SQLite，`refs/lattice/**`
    （SESS-01..05）。
  - **39 SQLite-to-Git Session Migration**：一次性迁移（MIG-01）。

**关键决策**（`.planning/STATE.md:62-82`）：v1.5 从 Phase 33 起；`CubeWorkspace` 二级 adapter
**已在 33 code-review 删除**（cube.rs 删除），**microsandbox 是唯一生产 adapter**；
RM-*（删 overlay/envelope）**故意延到 Phase 37**，在 Upper 执行路径（33–36）齐全之后，
"系统绝不能先丢掉执行边界"。

**并行线与 v1.5 的关系**：
- **LANCET / CEP**：**[已实施]** 的成熟子系统（`crates/lancet-*`，`lancet-native` 单独
  35,576 行）。10 个 CEP 方法、policy/Guard/effect/audit、capability envelope。**v1.5 不重写
  它，是在它之上换执行衬底**（Phase 34 路由、Phase 37 删 envelope）。`AGENTS.md` 的分层
  `lancet-protocol → … → lattice-api → tui/cli` 是当前真实架构（不是上一层 `/home/astrin/Dev/CLAUDE.md`
  描述的旧多 repo / plugin 结构——那个**已过时**，`lattice-tooling` crate 已 retire，
  commit `1ee885fb`）。
- **microVM / microsandbox workspace adapter**：**就是 v1.5 Phase 33 的产物**
  （`crates/lattice-workspace`）。MEMORY 里"摒弃 overlay 改 microVM""workspace adapter 用
  microsandbox SDK"即此线。
- **TUI（opencode parity）**：独立线（`crates/lattice-tui`，多个 `*-tui-*` 分支），**与
  uninode 适配无直接关系**，本报告不展开。
- **lattice-graph 分支**：**空壳**——`git log main..lattice-graph` 无领先 commit，无图代码。

---

## 2. uninode 设计读懂后的"增量"清单

把 uninode 三文档拆成"现有 / v1.5 已排期 / 纯新增"三类，才能谈适配度。

| uninode 设计元素 | 与现状关系 |
|---|---|
| 控制面(host) / 数据面(sandbox) 两层 | **同构** v1.5 Upper/Lower（`uninode-spec.md:7-48`） |
| 读走本地视图、写走 sandbox upper | **同构** v1.5（read=Lower-host, write/exec=Upper） |
| host-owned promote（CEP validate → PASS/REJECT） | **同构** v1.5 Phase 35 Promotion，但**机制不同**（见 §4 冲突②） |
| `.uninode/` 独立 git 仓库 + 三链同步 | **纯新增 [仅设计]**（`uninode-spec.md:263-340`） |
| fanotify 事件流 sandbox→host 建图 | **纯新增 [仅设计]**，且与现 adapter 接口冲突（见 §4 冲突①） |
| 一切皆 node（File/Operation + NodeKind/EdgeKind 注册表） | **纯新增 [仅设计]** |
| context compiler（Seed→Expand→Rank→Pack） | **纯新增 [仅设计]**，替换/包住当前 `build_messages`（见 §3 契合④） |
| 抽取优先 / Diagnostic 边 / bundle extractor | **纯新增 [仅设计]** |
| 学习型边权（ops trace ⊕ 静态先验） | **纯新增 [仅设计]**，**文档自标最大风险，(a) 不碰** |
| CEP 四边界（context/authority/audit/lifecycle） | authority/audit/lifecycle **概念上已实施**于 LANCET；context 边界部分新 |

---

## 3. 契合点（uninode 能复用/包住现有组件）

### 契合① 数据面 ≈ `crates/lattice-workspace`（强契合，但有接口缺口）
uninode 的"数据面 = sandbox，接口只有 `exec(prog,args)` + event stream"
（`uninode-spec.md:28-34`）几乎就是 `Workspace` trait 的设计意图。**已实施**部分：
- `Workspace::exec_upper(handle, prog, &[&str])`（`crates/lattice-workspace/src/lib.rs:360-365`）
  = uninode 的 host→sandbox `exec`，且**已强制 argv slice、禁 `shell()`**（T-33-05），
  正合 spec "exec(prog,args)" 而非 shell。
- `create_upper` / `restore_upper`（从 snapshot CoW）/ `template_create` / `destroy_upper`
  = uninode Setup 阶段的 "restore last good snapshot / cold boot"
  （`uninode-spec.md` Setup、`lattice-cep-sequence.mmd:19-27`）。
- egress 三态 `Open/Allowlist/Blocked`（`lib.rs:102-113`）= spec "boot(base_image, env,
  egress_policy)"。
- 凭据 by-reference + 快照不含密（CRED-01, `lib.rs:125-174`, T-33-04）。

**缺口（见 §4 冲突①）**：trait **没有 event-stream 方法**，`exec_upper` 是请求/响应、
缓冲 `stdout()/stderr()`（`microsandbox.rs:602-608`）。uninode 数据面的另一半（fanotify
JSONL 流）**无对应物**。spec 的 fanotify "实测验证"（`uninode-spec.md:243-259`）是一次
**独立 microsandbox 探针**，没进这个 adapter。

### 契合② CEP 边界语义 ≈ 已实施的 LANCET（概念契合）
uninode "CEP 四边界"（`uninode-spec.md:379-421`）里：
- **authority**：**[已实施]**——`lancet-authority` 的 capability/Guard L1/effect-region
  （`crates/lancet-authority/`，`AGENTS.md` Layer 2）。
- **audit**：**[已实施]**——`lancet-audit` 持久化 + provenance（`crates/lancet-audit/`）。
- **lifecycle**（build/test 失败可重试、baseline 漂移不可重试）：**部分已实施**——
  v1.4 的 candidate verification / lower-drift gate（`PROJECT.md:46`），v1.5 Phase 35
  PROMO-07 verify 绑定 `candidate_oid`。
- **context**（改动是否在声明范围内）：**部分已实施**——`handle_run` 里 WorkspaceMutation
  的 out-of-scope-paths 检查（`crates/lancet-native/src/handlers/command.rs:543-560`），
  但 uninode 想要的是**编译期声明的波及面**（CodePlan may-impact），那部分是新的。

→ uninode 不需要重造 CEP，**可把"四边界"重新表述为现有 LANCET 校验的语义封装**。

### 契合③ promote = write-back，REJECT = 丢脏页（架构契合 v1.5）
uninode promote / 恢复梯子（`uninode-spec.md:344-431`、`context-compiler.md` memory-hierarchy）
的**意图**与 v1.5 Phase 35/36 一致：host 收 ChangeSet → 校验 → PASS 提交 / REJECT 回滚。
v1.5 已把这条做成**journaled、crash-recoverable、baseline CAS、原子 ref 事务**
（`REQUIREMENTS.md:27-37`）——比 uninode 文档的"三链同步、每步补偿"
（`uninode-spec.md:313-338`）**更具体、更硬**。**uninode 应直接采用 v1.5 的 promotion，
而不是自带一套三链同步**（机制差异见 §4 冲突②）。

### 契合④ context compiler 能干净地包住当前 `build_messages`（高契合，零阻力）
**关键发现**：当前上下文组装**极简且无对手**。
- `build_messages`（`crates/lattice-agent/src/agent_loop/mod.rs:1036-1065`）= system_prompt
  + 完整 transcript + 当前 user_input，**仅此而已**。
- `system_prompt` 来自 **profile 静态字段**（`crates/lattice-api/src/turn/mapping.rs:14-21`,
  `lp.system_prompt`），不含任何文件内容 / 仓库上下文。
- **全仓无 RAG / embedding / 向量检索 / 知识图 / 文件自动注入**（grep `blake3|embedding|
  pagerank|vector.search|knowledge.graph` 命中全是 audit/overlay 误报；§见调查）。模型要看
  文件，只能自己发 `observe.workspace` tool call 现读。

→ context compiler 的落点**天然干净**：它产出一段"编译好的 context"，作为额外
system/context message 注入 `AgentTurnRequest`，**不与任何现有选择逻辑打架**（因为没有）。
代价对称：**也没有任何现成召回/读文件代码可复用**，compiler 的 Seed/Expand/Rank/Pack
四阶段要从零写。

### 契合⑤ 读路径已是"host 从本地视图返回"
uninode "read 由 host 从同步视图直接返回（view=local），不绕 sandbox"
（`uninode-spec.md:368-370`）——`observe.workspace` / read-file handler **已实施**为
host 侧 `tokio::fs::read_to_string` 读 `project_root`
（`crates/lancet-native/src/handlers/observation.rs:111-121,464`）。compiler 的
materialize 阶段（从 lower 读内容）可复用这条 host FS 读路径。

---

## 4. 冲突 / 张力（与现有架构假设打架处）

### 冲突① 数据面接口：事件流 vs 请求/响应 exec（**硬，且是 (a) 的核心依赖**）
- **设计**：uninode 数据面靠 **fanotify 守护进程在 sandbox 内、JSONL 事件流式回传 host**
  建 Operation+edge（`uninode-spec.md:198-234`）。
- **现状**：`Workspace` trait **无 event-stream / 无 streaming stdout**；`exec_upper` 一次性
  返回 `ExecOutput{exit_code,stdout,stderr}`（`lib.rs:223-229,360-365`；`microsandbox.rs:602-608`
  缓冲取 `out.stdout()`）。
- **张力**：要落 fanotify 数据面，必须**扩 `Workspace` trait**（加流式接口）或在 Upper 里
  跑 fanotify daemon 并多路复用进 exec 的 stdout 流（spec 设想的 "JSONL via exec stdout
  stream"，`uninode-spec.md:213`）。这是 trait 形状变更，触及 Phase 33 已冻结的契约。
- **更要命的前置**：fanotify 要抓到**有意义**的 agent 文件事件，**前提是命令真的在 Upper
  里执行**——而 `exec.command_run` **现在仍在宿主跑**（`handlers/command.rs` 的
  `execute_argv`/overlay supervisor，全是 host 路径），**把 exec 搬进 Upper 是 Phase 34
  (EXEC-01) 的活，尚未开始**。**所以 (a) 切片要试探的 fanotify 可靠性，实际上被 Phase 34
  阻塞**：Phase 34 之前，sandbox 内没有 agent 触发的写/exec 事件可抓。

### 冲突② "CEP" 语义漂移：envelope/实时 promote vs v1.5 删 envelope + Git CAS（**硬**）
- **设计**：uninode spec 的 CEP 仍带 `session.envelope` 心智，promote 是 turn 末"collect
  ChangeSet → CEP validate"的实时校验（`uninode-spec.md:344-366`）。
- **现状 + 计划**：(i) `session.envelope` **[已实施]** 且 `handle_run` 的 WorkspaceMutation
  **强依赖 active envelope + RunCommand capability**（`handlers/command.rs:124-227`）；
  (ii) 但 **v1.5 Phase 37 (RM-02) 要删掉整个 envelope 概念**，catalog 收到 9 方法，
  authority 改为"从不可变 config + per-call facts 直接推导"（`ROADMAP.md:110-121`,
  `REQUIREMENTS.md:53`,设计契约 §1/§5）。(iii) promotion 不再是"实时 validate 改动范围"，
  而是 **candidate Git 对象校验 + baseline compare-and-swap + verify 绑 OID**（Phase 35）。
- **张力**：uninode 文档若按现写法落代码，会**绑死一个 v1.5 正在拆除的 envelope/实时-validate
  模型**。**必须把 uninode 的"CEP 四边界"重映射到 v1.5 的 post-envelope、Git-CAS promotion
  上**，否则 uninode 与 v1.5 在同一块（authority/promote）正面相撞。

### 冲突③ source of truth：图 vs project repo（**软，spec 已自洽，但须守住**）
- context-compiler 的 memory-hierarchy 把 `.uninode/` 图当 "SSD"、promote 当 write-back
  （`context-compiler.md:27-35,182-204`），容易滑向"图是权威"。
- v1.5 与 uninode-spec **都已明确** project repo = source of truth、图可从 project reconcile
  （`uninode-spec.md:337-338`）。**结论：无真冲突，但实现时图必须始终是 project 的
  可重建投影**，不能让图状态成为 promote 的前置真相，否则与 v1.5 "Lower=Git worktree、
  promotion 是唯一写 Lower 路径" 的 source-of-truth 冲突。

### 冲突④ 模块依赖方向能否容纳 uninode（**需新增层，非冲突但要定位**）
现行分层（`AGENTS.md`）：`lancet-protocol → catalog → dispatch → audit/authority → native
→ lancet-api → lattice-api → tui/cli`；外加 `lattice-llm / session / profile / agent /
workspace`。**没有"图层"，也没有任何 crate 依赖 `lattice-workspace`**（grep 确认：除一处
doc 注释外零引用）——**workspace 现在是个孤立、未接线的 crate**。
- compiler 需要读 session/transcript（活跃集种子）、调 `lattice-workspace`（数据面）、
  产出注入 `lattice-agent` 的 context。最干净的落点是**新建 `lattice-uninode` crate**，
  位于 `lattice-agent` 同层或之上、`lattice-api` 之下，**单向依赖 workspace + session**，
  不反向污染 LANCET 协议层。
- 风险：若 compiler 直接塞进 `lattice-agent::build_messages`，会让 agent crate 长出
  图/IO 依赖，破坏"agent 只做循环、注入 LLM/tool traits"的薄度
  （`AGENTS.md` lattice-agent 角色）。**应让 compiler 在 `lattice-api` turn 编排层调用、
  把结果作为 `system_prompt`/context message 喂进去**，保持 agent 薄。

### 冲突⑤ 进度错位：(a) 想要的衬底还没建好（**时序张力**）
(a) = base bundle + fanotify 建图 + 无学习编译器。其中：
- "fanotify 建图" ⟂ exec 还在宿主（Phase 34 未做）→ 见冲突①。
- "无学习编译器跑一个 turn、量化 vs 朴素读文件" ⟂ 现在根本没有"朴素读文件进 context"
  这个 baseline——模型靠 tool call 现读（契合④）。所以对比基线要么是"模型自主
  observe.workspace 的 round 数/token"，要么得先搭一个朴素 baseline。

---

## 5. v1.5-alpha 具体适配：uninode 是 v1.5 的一部分吗？

**不是。** 证据：
- uninode 三文档**不在** `.planning/REQUIREMENTS.md` 任何 REQ-ID 下，**不在** Phase 33–39
  任何 phase 的 Success Criteria 里。它们是 `docs/` 下的独立 DRAFT（spec 2026-06-16、
  compiler 2026-06-18、prior-art 2026-06-19），由 `docs/uninode-context-compiler` 分支承载，
  **未排期**。
- v1.5 的范围被 `PROJECT.md:61-71`/`REQUIREMENTS.md:65-84` 明确钉死为 A+B（Upper 执行 +
  overlay 删除 + Git promotion；Git session + 迁移）。图 / context 编译 / 学习边权
  **均未列入，亦未列入 v1.5 的 Deferred**——即它们是 **v1.5 之后/之外**的方向。

**会不会重复或打架？**
- **打架**：与 Phase 37（删 envelope）在 CEP 语义上打架（冲突②）；与 Phase 33 冻结的
  `Workspace` trait 在数据面接口上打架（冲突①）。
- **重复风险**：若 uninode 自带"三链同步 / 实时 promote validate"，会与 Phase 35
  Promotion **重复造轮子**——应复用 v1.5 promotion，不另起。
- **共生**：uninode 的图+编译器层，是 v1.5 执行衬底**之上**的纯增量，二者**可分层共存**——
  前提是 uninode 不下沉去改 promote/envelope，只在"context 注入 + 旁路建图"层活动。

**(a) 能搭在当前进度（Phase 33 完成）上吗？**
- **部分能、部分要等**：图骨架、`.uninode/` JSONL 读写、Seed-Expand-Rank-Pack 编译器、
  把编译结果注入 `AgentTurnRequest`——**这些不依赖 Phase 34+，现在就能写**（契合④⑤都成立）。
- **fanotify 建图这一半要等 Phase 34**（EXEC-01 把 exec 搬进 Upper），否则 sandbox 内没有
  agent 文件事件。**建议把 (a) 拆两步**：(a1) 离线扫描建图（scanner，不靠 fanotify）+
  无学习编译器 + 注入 + 量化 context 质量（**现在可做**）；(a2) fanotify 实时建图探针
  （**Phase 34 落地后**，在真有 Upper 执行时验证 fanotify 可靠性与伪依赖率）。

---

## 6. 缺口 / 前置依赖（要落 (a) 还缺什么）

| 缺口 | 类别 | 说明 / 出处 |
|---|---|---|
| `Workspace` trait 无事件流接口 | 接口缺口 | `lib.rs:348-389` 仅 exec/snapshot；fanotify 流无落点（冲突①） |
| `exec.command_run` 仍在宿主执行 | 前置(Phase 34) | `handlers/command.rs` 全 host 路径；EXEC-01 未做 → fanotify 无事件可抓 |
| `WorkspaceSection → UpperSpec` 翻译未接线 | 前置(Phase 34) | M-33-01 / D-33-01（`FINDINGS.md:15-19`）；Phase 33 只给了 schema |
| `lattice-workspace` 未被任何 crate 消费 | 接线缺口 | grep 确认零引用；turn 编排层还没调 adapter |
| 无图存储 / NodeKind/EdgeKind 注册表 / scanner | 纯新增 | 全 [仅设计]，零代码 |
| 无 context 编译器（四阶段） | 纯新增 | `build_messages` 极简，无可复用召回逻辑 |
| 无 "朴素读文件" baseline 可对比 | 评测缺口 | 当前模型靠 tool call 现读，(a) 的对照基线需先定义/搭建 |
| Diagnostic 边需 ingest 期 parser | 纯新增 | exec 输出现为缓冲 blob，无结构化诊断抽取 |
| CEP 四边界尚未"重映射"到 post-envelope 模型 | 设计前置 | 须等/对齐 Phase 37 的 9-方法 + config-derived authority（冲突②） |

---

## 7. 落点建议（(a) 切片具体怎么搭）

**定位**：把 (a) 明确为 **"v1.5 之后/之外的旁路探针"**，**只读、不写 Lower、不碰 envelope /
promote / session**。这样它与 v1.5 完全解耦，可在 v1.5 推进的同时独立验证，互不阻塞。

**落点**：新建 `crates/lattice-uninode` crate。
- **依赖方向**（守 `AGENTS.md` 单向分层）：`lattice-uninode → lattice-workspace +
  lattice-session + lattice-llm(types)`；**被 `lattice-api` turn 编排层调用**，产出注入
  `AgentTurnRequest.system_prompt` / 额外 context message。**不依赖、不修改任何 `lancet-*`
  协议/native 层**——避免与 Phase 37 的 CEP 改动耦合。
- **不要**把编译器塞进 `lattice-agent::build_messages`（会破坏 agent 薄度，冲突④）。

**复用**：
- `crates/lattice-workspace` 的 `Workspace`/`MicrosandboxWorkspace`/`DeterministicWorkspace`
  作数据面（exec + snapshot）。
- `observation.rs` 的 host FS 读路径思路，做 compiler materialize（从 lower 读内容）。
- `lattice-session` 的 transcript 作"活跃集"种子来源（最近 turn 的 ops）。
- DeterministicWorkspace 让 (a1) 的离线建图 + 编译 + 量化能在 **CI / 无 KVM** 跑。

**新增（(a1)，现在可做）**：
1. `.uninode/` JSONL 读写 + manifest（独立 git 仓库，`uninode-spec.md:263-311`）。
2. base bundle：`File`/`Operation` 节点 + `Read/Create/Modify/Delete/Contain` 边；
   `id=blake3(kind‖path)`；NodeKind/EdgeKind 注册表雏形（**先 base，不做 domain bundle**）。
3. **离线 scanner**（遍历项目文件建 File 节点 + Contain 边），**不靠 fanotify**。
4. 无学习编译器：Seed（先只做"显式引用 + 活跃集 + 常驻"，**砍掉 embedding/语义命中以
   保持最小**）→ Expand（静态边权 best-first 扩散，**纯静态先验，无学习**）→ Rank
   （`R×(1+α·recency+β·idf)`，可先只留 R + idf）→ Pack（保真度降级背包 + KV-cache 排序）。
5. 注入：把编译结果作为额外 context 喂进 `AgentTurnRequest`，跑一个 turn。
6. 量化：对照"模型自主 observe.workspace 的 round/token" 或一个朴素 head-of-files baseline，
   测 context 质量 / token 利用率。

**新增（(a2)，等 Phase 34）**：
7. fanotify daemon-in-Upper 探针 + `Workspace` 流式接口扩展（或 exec-stdout 多路复用），
   验证实时建图可靠性、virtiofs 高并发下伪依赖率（`uninode-spec.md:251` 未压测项）。

**明确不碰**（守住边界）：
- 学习型边权（`context-compiler.md:218-253` 自标最大风险）——(a) 全程纯静态。
- promote / 三链同步 / envelope——复用或等 v1.5，不自造。
- domain bundle（code/web）、Diagnostic 边、Summary 节点——(a) 之后再说。

---

## 8. 最大的冲突 / 风险（一句话各一条）

1. **fanotify 数据面被 Phase 34 阻塞**：(a) 最想试探的 fanotify 可靠性，依赖"exec 真在
   Upper 执行"，而那是 Phase 34（未开始）的活——**(a) 的 fanotify 半截要么等 Phase 34，
   要么先做离线 scanner 版 (a1)**。
2. **CEP 语义与 Phase 37 正面相撞**：uninode 文档仍带 `session.envelope` / 实时 promote
   validate 心智，v1.5 Phase 37 正要删 envelope、promote 改 Git baseline CAS——**uninode
   落代码前必须把"四边界"重映射到 post-envelope、9-方法、config-derived authority 模型**。
3. **接口契约变更成本**：`Workspace` trait（Phase 33 刚冻结、KVM 验证过）无事件流，加流式
   接口是契约变更，需协调，不能在 uninode 侧偷偷扩。
4. **评测基线缺失**：当前无"读文件进 context"的 baseline，(a) 的"vs 朴素读文件"对比需要
   先把对照组定义/搭出来，否则量化结论站不住。
5. **学习边权（已知最大技术风险，(a) 已规避）**：成本不对称（LLM 缺页=整个推理 round-trip）、
   坏 trace 自我强化、无纠偏机制——(a) 明确不碰是对的；真正投入前需独立 spike。

---

## 附：本报告的关键证据索引

- v1.5 范围/进度：`.planning/ROADMAP.md:9,22-162`、`.planning/STATE.md:1-83`、
  `.planning/REQUIREMENTS.md:14-117`、`.planning/PROJECT.md:21-71`
- v1.5 设计契约（含 Upper/Lower、删 envelope、Git session）：
  `docs/archive/designs/2026-06-13-cube-upper-git-session-design.md`（§1/§5/§6）
- 当前架构/分层：`AGENTS.md`（分层表、Crate Roles、Turn Execution）
- 数据面 adapter（exec-only，无事件流）：`crates/lattice-workspace/src/lib.rs:223-389`、
  `crates/lattice-workspace/src/microsandbox.rs:524-623`
- 上下文组装极简（无 RAG/图）：`crates/lattice-agent/src/agent_loop/mod.rs:1036-1065`、
  `crates/lattice-api/src/turn/mapping.rs:14-31`
- read 路径走 host FS：`crates/lancet-native/src/handlers/observation.rs:111-121,464`
- exec 仍在宿主 / envelope 依赖：`crates/lancet-native/src/handlers/command.rs:72-227,444-510`
- Phase 34 缺口台账：`FINDINGS.md:15-25`（M-33-01/02）
- workspace 未被消费：grep `lattice_workspace` 全仓仅 `crates/lattice-profile/src/schema.rs:61`
  一处 doc 注释
- uninode 设计：`docs/archive/uninode/uninode-spec.md`、`docs/archive/uninode/context-compiler.md`、`docs/archive/uninode/uninode-prior-art.md`
- CEP 序列图用 v1.5 术语：`docs/archive/uninode/lattice-cep-sequence.mmd:1-6`（"lower · middleware" /
  "upper · agent"）
