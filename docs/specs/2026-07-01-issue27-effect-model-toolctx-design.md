# Issue #27 模型设计 —— Effect 模型(R7a/R8)+ 工具 substrate + 通道模型 + ToolHost

日期:2026-07-01;第四轮修订 2026-07-02(工具后端结构规范讨论,原 4 项待拍全部落定);
GitHub #65(原 Codeberg #43)词汇对齐修订 2026-07-22(物理假设对齐 ADR 0003 Direct-Local + ADR 0004 Tool Host / WASM Component)。

状态:由 brainstorming 讨论产出(2026-07-01),2026-07-02 与用户逐项拍板后第四轮修订;**待用户复审定稿**。
取代现 Issue #27 正文的模型部分(该票被标 `need-major-revision`)。派生自 PRD #24(crate 边界重构)及其
R3/R7/R8/R9 处置;现行物理与承载边界以 **ADR 0003(Direct-Local)** 与 **ADR 0004(Tool Host /
WASM Component)** 为准。本文档只覆盖 Issue #27 所立的**模型**:effect 类型核、工具 substrate
类型、通道模型、`ToolHost` 宿主面。不迁移生产工具(#29–#32)、不退役 authority 里的 `ImpactKind` 机器
(#28)、不合并 crate(#34)、不建工具 CLI(#35)。

> **核心:工具从框架解耦、可单独维护;框架只保留小而精的数据流控制。**
> 框架不理解工具的领域语义,只拥有三样:准入(capability)、通道(数据流的物理路径)、记账
> (effect 证据 + context 度量)。本文档一切结构决定以此为最终判据。

## 1. 摘要

Issue #27 给 LATTICE 的源码 crate 工具系统铺横向地基:封闭 effect 核、
`Tool`/`ToolDescriptor`/`ToolHost` substrate、由 manifest 生成的 host 侧 `inventory` 注册、
`lattice-tool-prelude`,以及一个吸收 dispatch registry 与 transport 的新 crate `lancet-runtime`。
本切片不迁移任何生产工具;substrate 由一个 **no-op 测试工具**打穿验证(PRD 测试决策),而不是抽取
真实 handler。

两条轴已被 PRD #24 敲定,这里作为"已定背景"重述:**effect 类型核**(封闭 `ctx-op × ext-impact` + 开放的工具定义 scope + fail-closed 的 `in`/`ex`,单轴 capability 准入,cell-key 表示)与 **R8 的 `FailureSemantics` 清理**(§5.4,删整个枚举)。真正待设计、也正是 #27 被判 major-revision
的原因,是**`ToolHost` 宿主面**:工具能碰什么、强制怎么中介、以及怎么把 agent 工具面跟系统其余
API 分开。这些是本文档的实质贡献(§6–§8),外加一条贯穿后续 issue 的结构性纠正:**audit 是运维
基础设施,不是 agent 工具**(§7、§10)。

第四轮把宿主面定为**通道模型**(§6):框架只认三类封闭通道
(`workspace`/`network`/`session`),每条通道配一个强制装饰器——这是**操作级收口**(框架看得懂每个
操作,逐个管)。原「模式 B / 域宿养」(把领域后端进程跑进隔离执行域)已由 ADR 0004 **superseded**
(禁止 hosted backend processes;若再引入需新 ADR,§6.2)。网络轴对齐 Direct-Local:`fetch.web`
字节从宿主侧 Tool Host `network` 通道发出,强制面 = `net-guard` + `[lancet.network].allowed_hosts`
(§6.6)。承载面以 ADR 0004 为准:**每个 Tool(含五个内建)作为 WASM Component 执行**;`inventory`
保留为生成的 host 侧 Component 注册,不是长期生产模型下的「进程内 trusted-plugin 承载」
(§6.7–§6.8)。逻辑规范(cell / capability / 通道契约 / 三层强制)跟平台无关、写死。

## 2. 目标

1. 在 `lancet-protocol` 里把封闭 effect 核与开放 scope 模型定义为纯数据类型,替换掉
   `ImpactKind`/`EffectScopeKind`/`OverlayBehavior`/`FailureSemantics`。
2. 定义 `Tool` trait、`ToolDescriptor`、`ToolHost`,让一个工具就是一个独立维护的 Source Tool,
   经 manifest 生成的 host 注册进入 runtime,且 runtime 库不依赖任何具体工具 crate。
3. 让 `ToolHost` 成为**受控 facade**:它各通道句柄(workspace/network/session)的中介路径在构造时
   就带上强制,任何工具都不必自己重抄准入/budget/effect 记录。
4. 确立 **agent 工具 catalog 是投影面,不是系统的调用 API** —— operator/程序化消费者直接绑到
   store/backend 库。
5. 用一个 no-op 测试工具打穿 substrate:生成注册 → raw 注册 → `CepClient` round-trip →
   `ToolHost` → audit 行。
6. 定义 `ToolDescriptor` ↔ `manifest.toml` 一致性(格式 + drift 测试),让工具迁移票
   (#29–#32)能满足各自"manifest drift check 通过"的验收。

## 3. 非目标

1. 迁移任何生产工具(`session`/`observe`/`mutate`/`fetch`)—— 见 #29–#32。
2. 删除 `lancet-native` 或 `register_native_*` handler —— 它们留作过渡;彻底删除是 #34。
3. 重写 `ImpactKind`-keyed 的 authority 机器(`effect/region.rs`、`authorized_region_for`)——
   那是 #28 里的 R7b。
4. 真正的 `ContextStore` 或任何 `Free` 执行机制 —— `Free` 本期只到词汇(PRD R10)。
5. `lattice tool install/list/uninstall` CLI 与 release-gate 接线 —— #35(本期只落 manifest
   **格式** + drift **测试**;见 §9)。
6. 重新引入 hosted backend processes / 任意进程执行 —— ADR 0003/0004 明确禁止;若再引入需新 ADR
   (§6.2)。WASM Component 承载与零 ambient WASI 是 ADR 0004 的现行目标,不在本切片排期细节,
   但本模型以之为权威物理边界(§6.7)。

## 4. 背景(已核验的当前状态)

- `lancet-protocol/src/effect_types.rs` 仍定义 `ImpactKind`(:22)、`EffectScopeKind`(:81)、
  `OverlayBehavior`(:97)、`FailureSemantics`(:110)四变体齐全,含 `validate_complete` 的
  command 规则(:141)。
- `CepMethod` 仍带 `const IMPACT`(`lancet-catalog/src/method.rs:23`)与 `BASE_AUTHORITY`(:26);
  `MethodDescriptor.impact`(:37)。
- `HandlerContext`(`lancet-native/src/server/registry.rs:23-42`)给 handler **15 个字段**
  (events、cancellation、`cep_context.budget`、`effect_ledger`、`authorized_region`、
  `policy_hook`、`audit`、`durable_metadata`、`execution`、`routing_invocation`…)。准入/budget/
  effect 记录是**handler 自驱**:`evaluate_and_enforce_policy`(guard_l1)由 8+ 处手动调用;
  `todo_write` 自扣 budget、自记 effect(`session_state.rs:28,36,43`)。runtime 只强制 audit
  生命周期(`transport.rs:130`/`:199`)与 dispatch 后 effect 校验(`apply_effect_check`,
  `transport.rs:177`)。
- 注册被 `CatalogMethod` 绑死(`lancet-dispatch/src/registry.rs:118-120`);agent adapter 走
  硬编码 8 臂 match + catalog 成员闸门(`lattice-api/src/turn/cep.rs:110-121,151`)。一个
  经 `inventory` 注册的工具目前 agent 触达不到。
- `lancet-runtime` 与 `lattice-tool-*` crate 不存在。工具知识仍在框架里:
  `lancet-authority/src/policy/network.rs`(fetch)与 `observation.rs`(observe)。command 知识
  已随 #26 删除。
- 物理执行已切到 Direct-Local(ADR 0003):LANCET 直接操作 Local Workspace;turn-scoped 隔离镜像 /
  Upper 生命周期 / egress 网关路径已随 #62 删除。网络策略留在 `[lancet.network]` + `net-guard`。

## 5. Effect 模型核心(R7a / R8)—— 已定(PRD),重述

### 5.1 封闭核(框架词汇的全部)

两条正交轴 + 一个信任属性 —— 框架只拥有这些:

- **`CtxOp` = `Alloc | Free | Mutate`** —— 操作对 agent context 的影响,context 像**内存**一样管。
  `Alloc` = 结果进 context;`Free` = 释放/逐出(让 context 能收缩的新 primitive);
  `Mutate` = 改已有 context 状态。
- **`ExtImpact` = `None | Read | Write`** —— 操作对外部世界的影响。
- **`in` / `ex`** 信任边界属性(只作语义,对齐 ADR 0004):`in` = 由 Tool Host 域可控
  (经 Tool Host 通道中介、可记账);`ex` = 外溢到外部世界(例如跨出 host 控制面的网络效应)。
  **不是**已退役的隔离执行域 / promotion / sandbox containment 语义。

> **一次调用可产生多个 cell。** 操作本身的 CtxOp(`Mutate` 改已有状态 / `Free` 逐出)与返回值进
> context 的 `Alloc` 是两回事,分别记 cell。每个工具调用至少有一个"返回值进 context"的
> `(Alloc, …, scope)` cell(budget 扣这侧);若操作还改了已有状态,另记一个
> `(Mutate, …, scope)` cell(操作侧,**不扣 budget**);若释放,另记 `(Free, …, scope)`。例:
> `session.todo_write` 产生 `(Mutate, None, session/in)`(改 todo 状态)+ `(Alloc, None,
> session/in)`(返回的 `TodoWriteResult` 进 context,扣 budget)——这与现状
> `session_state.rs:95` 的 `budget_charged: 0` 一致(操作侧不扣),返回值 Alloc 扣费是 #27
> 要补的(budget 上移到 runtime 事后量返回值,§6.5 第 3 层)。读 workspace 是
> `(Alloc, Read, workspace/in)`(读即 Alloc,无独立 Mutate)。`fetch` 是
> `(Alloc, Read, external-host/ex)`。每个 cell 两个轴都填;一次调用是若干个这种 cell 的集合,
> 矩阵是稠密的,不是 tagged union。

> **budget 只记 `Alloc`,不是 token 预算。** 现状(`ObservationBudgetTracker`,
> `lancet-authority/src/policy/budget.rs`)按返回字节 + op 计数(`max_grep_matches`/
> `max_list_entries`/`max_find_paths`/`max_todo_ops`)+ 单读上限(`max_single_read_bytes`)扣费——
> 本质都是 `Alloc` 的字节日代理,而且框架替每个工具各定一种量,没有统一口径(PRD problem #24 的
> budget 版)。#27 改成 **CtxOp 轴上的单一 context 预算标量**:`Alloc` 扣、`Free` 退;每个工具把
> 自己的输出裁到该裁的大小是工具的 mechanism,不是框架 policy。ExtImpact 那条轴不走预算,走
> 准入(capability + `in`/`ex` + `ObservedScope`)。框架因此只剩两样:一把 context 度量 + 一条
> 准入轴。**度量由 runtime 在事后量返回值的大小**(不信工具自报)——量的是进了共享 context 的那
> 份返回值,不是工具内部读了多少字节。成本(context 占用)记给 `Alloc`,收益(逐出低相关内容)记
> 给 `Free`,两者正交。度量单位 = 参考 tokenizer(o200k_base,§11.8 决议记录,2026-07-19 修订)的 token 数,真实 token
> 用 provider `usage` 回校。扣费在 §6.5 第 3 层(事后),不在第 2 层逐操作扣。

### 5.2 开放 scope 轴

第三轴是 **scope**——一组由工具自己起名的字符串标签(开放集合,框架不枚举)。框架不在乎 scope
叫什么、什么意思;但每个 scope 声明必须带两个框架要用的坐标:

- **`trust`**:`in` / `ex`(§5.1 信任边界属性);
- **`channel`**:该 scope 的效果的**物理落点**——某个模式 A 通道(`ChannelKind`)。历史上还可标到
  已 superseded 的模式 B 域宿养标记(§6.2);现行生产只认三条模式 A 通道。这是 capability ↔ 通道
  绑定链的锚点:`capability → cell → scope → channel`(§6.3)。

**双重 fail-closed**:没声明的 scope 一律算 `ex`;且没声明的 scope **没有物理落点**——谁要是引用
了它,注册/供需校验时直接拒(§6.4),不降级放行。一个被漏掉的、新加的、没审计的 scope 因此既不会
被悄悄信任,也不会悄悄拿到 I/O 路径。

这样 scope 的开放性就有了精确含义:**名字(语义)开放,工具随便起;物理落点封闭,必须落到框架的
通道枚举上**。一个叫 `sqlite-db` 的 scope,效果物理上落 `workspace` 通道(db 文件在 Local
Workspace),cell 记 `(Mutate, Write, sqlite-db/in)`——审计词汇是工具的,强制路径是框架的。

### 5.3 capability、cell、准入

- **基数:** 一个 capability **只根植于一个 cell**(`cell = (ctx-op, ext-impact, scope)`,manifest 里
  `[[capabilities]].cell` 是单数);**多个 capability 可以共享同一个 cell**——比如
  `inspect-workspace` 和 `read-content` 都落在 `(Alloc, Read, workspace)`(§9.1)。所以从 cell 看
  capability 是一对多,不是"capability 包含多个 cell"。一个 capability 名固定它那个 cell 的全部
  坐标(含 trust)。
- **授权只有一条轴:capability 准入。** 一个 turn 准入一组 capability(一组名字)。粗门只是对这组
  名字的谓词——`max_boundary`(默认 `in` = 拒绝所有 `ex`)、"deny all write" 之类——不是额外多一条
  匹配轴。再叠上运行时 `ObservedScope`(path/host)校验。没有单独的 `base_authority`(R9 已吸收)。
- **两段式授权(把旧的 ALL/ANY 冲突收掉):**
  + **工具可调** ⟺ `descriptor.capabilities ∩ allowed_caps ≠ ∅`(至少准入一个它声明的 capability)
    + `max_boundary` 通过(deny-`ex` 的 turn 拒绝任何 `ex` cell 的 capability)。**不是"全部都要
    准入"**——同 cell 的两个 alias 准入任一即可;带可选 `ex` capability 的工具在只授权 `in` 时
    仍可调,只是 `ex` 操作会被第 2 层挡下。
  + **通道句柄可用**(§6.3)⟺ 准入的 capability 里有根植在它通道上某个 cell 的。
  + **单次操作授权**(第 2 层,§6.5):这次操作产生的 cell(含 `ToolOutput.result_cell` 那个
    Alloc cell)⟺ 已准入的 capability 里有一个根植这个 cell 的(ANY);没有 → **这次操作失败**
    (不是整工具被拒,工具还能调其他已覆盖 cell 的操作)。
- **物理落点:** cell 的 scope 经 `ScopeDecl.channel`(§5.2/§8)落到唯一一条物理通道(模式 A 的
  `ChannelKind`)。所以 capability **自带通道坐标**——准入一个 capability,
  框架同时知道它授权的是哪条通道上的哪类效果。这是 §6.3 发句柄、§6.5 第 2 层装饰器归因的依据,
  也从结构上杜绝了"capability 和通道两张独立表"。
- **记什么、记在哪(把"审计用 cell-key+capability"和"effect 只记 cell"两个口径调和):**
  cell-key `"ctx:ext:scope:in|ex"` 和 capability 名是**同一套词汇**,但**不同场景记不同字段**:
  + **调用准入记录**(runtime,dispatch 前):记**准入的 capability 名集**——这是"谁被授权"的事实。
  + **effect 记录**(执行期,通道装饰器):记**cell + `ObservedScope`**——客观发生了什么效果,
    不记 capability 名(§6.3:多 capability 同 cell 时归因无歧义)。
  + **attestation**:分字段含两者——cell 字段(发生了什么)+ capability 字段(凭什么授权)。
  + **profile**:用 capability 名(人写配置自然用名字)。
  #28 body 的 effect 记录机器重表达要同步这个口径(effect ledger 记 cell 不记 capability 名;
  capability 名只留在准入/attestation/profile)。
- `ToolDescriptor` 带 `capabilities` + `scopes`(**就等于旧 `EffectDecl` 的角色,不再有独立的
  `EffectDecl` 类型**;旧 catalog 的 `const EFFECT` 由 `const DESCRIPTOR` 取代)。`ScopeDecl`(scope
  名 + `trust` + `channel`)作为独立声明,被 cell 的 scope 名引用。返回值那个 Alloc cell 由
  `Tool::call` 返回的 `ToolOutput.result_cell` 报告(§8),runtime 校验它落在 descriptor 的天花板
  内——**descriptor 不固定 result_cell**,允许工具按 params 返回不同的 result_cell(比如 read_file
  报 `(Alloc, Read, workspace/in)`、todo_write 报 `(Alloc, None, session/in)`)。

### 5.4 R8 —— `FailureSemantics` 删除

**决议:删整个 `FailureSemantics` 枚举**(`effect_types.rs:110-115`)和 `EffectDecl.failure` 字段
(:129),含 `validate_complete` 的 command 规则(:141-144)。effect 校验只认 cell(§5.3 分字段:
effect 记 cell 不记 capability 名),不做静态 failure 承诺。依据:

1. **没有行为消费者**(全仓核验):引用面 = 枚举定义 + 10 个 `const EFFECT` 声明(分布在 5 个
   catalog method 文件 `methods/{session,audit,workspace,external,scaffold}.rs`,声明者不是消费者)
   + `validate_complete` 一条注册自检(它检查的 `PartialEffectsAudited` 本身就在删除清单里)。没有
   任何运行时逻辑读 `.failure` 改变行为——PRD R8"纯声明元数据"判断成立。
2. **名不符实**:Direct-Local 下的非原子写路径(例如历史 guest 写助手的两步 `mkdir` + 重定向)
   已说明「失败时无后端效果」类静态标签配不上真实残留语义;#37 删 `mutate.batch` 只消掉多操作
   partial failure,没证明单文件无残留。Atomic Local Mutation(ADR 0003)用安装前失败保持目标不变
   来处理,而不是用枚举承诺。
3. **跟效果观察模型矛盾**:效果以观察为准,不以声明为准;失败后的残留由第 3 层事后校验看见,
   而不是由枚举承诺它不存在。原模式 B「事后 diff」叙事亦同此理(该模式已 superseded,§6.2)。

`RetainDiffOnVerifyFailure` 随 #26 已死,`PartialEffectsAudited` 随 command 退役,其余两变体随枚举
整体删除。

### 5.5 删除项

删 `ImpactKind`、`EffectScopeKind`、`OverlayBehavior`、`FailureSemantics`(§5.4);
`CepMethod::IMPACT` 与 `BASE_AUTHORITY`;`MethodDescriptor.impact`。(`ImpactKind`-keyed 的 authority
机器的下游重表达是 #28,不是 #27。)

### 5.6 #27 精化 —— agent scope 集排除 `audit`

**已确认**(对 PRD 五工具集唯一的实质偏离,扫描证实 audit 是唯一被非 agent 消费的方法):agent 面
的 scope 集是 **`{ workspace, session, external-host }`**。**不**含 `audit`;agent 词汇里没有
`ReadAudit` capability,也没有 `(Alloc, Read, audit)` cell。理由见 §7、§10。(`observe`/`mutate` =
`workspace/in`;`session.todo_write` = `session/in`(操作 `Mutate` + 返回值 `Alloc`,§5.1);
`fetch` = `external-host/ex`。)

## 6. 通道模型与 `ToolHost` —— 设计(本切片核心)

### 6.1 原则:收的是效果域,不是单个操作

今天的 `HandlerContext` 是个"服务袋":把原始的 `policy_hook`、`budget`、`effect_ledger`、
`audit` 一股脑塞给 handler,由它**自己记得**去调 `guard_l1`/`try_charge`/`record_effect`。对在册
可信 handler 只是乱;对第三方工具就是 fail-open。`ToolHost` 把它反过来:工具拿到的是**受控通道
句柄**,不是原语。

框架**包不住所有后端语义**——rust-analyzer 这类领域逻辑不可能也不应该编译进框架 trait 的封闭枚举。
现行权威边界(ADR 0003 + ADR 0004)是:**封闭结构化 catalog + Tool Host 通道**;工具作为 WASM
Component 执行,唯一权威是 WIT Tool Host imports,没有 ambient 文件系统 / 网络 / 进程权限,也没有
hosted backend processes。逐操作中介(模式 A)是公共高频路径上对这个保证做的强制 + 记账。

### 6.2 双模收口(模式 A 现行;模式 B superseded)

**模式 A — 通道原语(操作级收口)。** 框架看懂操作语义,逐操作强制 + 记账。通道是**框架拥有的封闭
枚举**,现行 = 3 条:

- **`workspace`** —— 继承下来的 9 个方法(`read_file`、`write_file`、`remove_file`、`list_dir`、
  `grep`、`find_files`、`resolve_path`、`changed_paths`、`backend_name`;
  `execution_backend.rs:261-307`)。**没有 `exec`。** 维持不收窄(§11.7 决议记录)。
  Direct-Local 下这些方法落到 `lancet-native` 的 concrete Local Workspace 模块(ADR 0003)。
- **`network`** —— `execute(HttpRequest) -> HttpResponse`(请求级;形状参考 wasi-http
  outgoing-handler,语义自有)。fetch 是它的第一个消费者,但不是唯一目标消费者。见 §6.6。
- **`session`** —— session 状态存取(host 侧 store)。**现状:当前 `session.todo_write` handler
  没有 store,只验证 + 回显**(`handlers/session_state.rs:24-53`:`validate_todos` 后直接返回
  `TodoWriteResult { todos: params.todos }`,无持久化,`budget_charged: 0`)。host 侧 session
  store 在 #29(session tracer)落地时建;本切片只定 session 通道的**契约形状**与 CtxOp 语义
  (操作 `Mutate` + 返回值 `Alloc`,§5.1),不假装 store 已经在。当前 effect 记的
  `(Mutate, None, session/in)` 描述的是**目标语义**(#29 兑现:验证通过 → 写 store),现状是"通过
  验证、拟写入"的证据。这跟 `CtxOp::Free` 的"本切片只到词汇级"是对称的——通道契约先定,
  实现归迁移票(#29)。

  > **关于 workspace 9 方法的粒度(不是最小集,但模型层零冗余):** 这是**继承下来的 I/O
  > substrate**(PRD 只删了 `exec`,保留其余 9 个)。方法层确实有重叠(`list_dir` 是 `find_files`
  > 的 depth-1 特例等),但在 effect 模型层没有重复:`read_file`/`grep`/`list_dir`/`find_files`
  > 今天已经是同一个工具 `observe.workspace` 的 4 个 mode,共用一个 impact 和 capability
  > (`workspace.rs:19-41`),映射到新模型就是一个 cell `(Alloc, Read, workspace/in)` → 多个派生
  > capability。分层是:`通道方法(操作形状/返回类型/经 Tool Host 强制)→ 1 个 cell(授权单元)
  > → N 个 capability(工具派生)`。`resolve_path`/`changed_paths`/`backend_name` 是内部原语,
  > 不是 agent observe mode。

  **一条通道 trait 方法要进枚举的准则:它需要 Tool Host / Local Workspace / host store 特权,
  而且没法用既有原语安全又经济地组合出来。** 标定例:`grep` 进通道(组合实现要把每个文件拖过
  Component 边界,不经济到不可用);fetch 在 HTTP 之上的语义(参数→请求构造、响应整形、
  truncation)留给工具,"发一个受控请求"进通道。新工具默认组合既有通道;要扩通道 trait 必须过这条
  准则——否则框架又会重新累积工具领域知识(PRD problem #24 的 backend 版)。

**模式 B — 域宿养(域级收口)。状态:superseded(ADR 0004)。**

历史上,模式 B 曾设想「框架看不懂操作语义时,把工具自带的领域后端进程放进隔离执行域,只证
containment、不证完整 effect」。ADR 0004 明确禁止 **hosted backend processes**,因此该模式**不再
是现行选项**。若未来要重新引入任意领域后端进程或类似宿养模型,必须另开 ADR,不能从本字段形状
默示恢复。

保留点(仅形状,非活动路径):

- descriptor / manifest 字段 `hosted_backend` / `[hosted_backend]` 可作为 **reserved / superseded**
  形状留在类型与 prototype manifest 注释里,便于历史对照与 drift 测试不误读未知键;在 ADR 0004
  下**不得激活、不得作为供需校验成功路径**。
- 本切片不落地任何域宿养实现细节(无隔离域内 spawn、无 guest PATH 注入、无 warm-template 资产
  bake、无 virtiofs/fanotify 读观测依赖)。

模式 A 与(已 superseded 的)模式 B 曾是同一原则的两级投资:模式 A 用「预先理解语义」换操作级记账。
现行只保留模式 A;覆盖面扩展走「提升为新通道原语」或「在 Component 内用已有通道组合」,不走
hosted backend。

**权力不对称(规范条款):工具对通道只有需求声明权,没有实现提供权。** 工具不得接管框架的强制面
(不得自带 `network` 通道实现去替换 `net-guard` 路径);「后端跑在哪」也不是工具的自由度——
Component 只能经 Tool Host imports 影响世界(ADR 0004)。

### 6.3 `ToolHost` 面与 capability↔通道绑定

绑定链:**`capability → cell → scope → channel`**。capability 根植 cell(§5.3),cell 的 scope 坐标
经 `ScopeDecl.channel`(§5.2/§8)落到唯一一条物理通道。descriptor **没有独立的 `channels` 字段**
——工具的通道集是个**派生量**(全部声明 scope 的落点并集),供需校验(§6.4)和句柄发放都按这个派生
集来,不存在第二份会跟 capability 失同步的声明。

句柄在 `ToolHost` **构造期**(dispatch 前,作为第 1 层准入的一部分)发好,取用时无参:

- `workspace()` / `network()` / `session()` —— 模式 A 通道句柄(经 §6.5 第 2 层装饰)。句柄可用 ⟺
  派生通道集里有这条通道 **且** turn 准入的 capability 里有它的 cell.scope 落在这条通道上的;否则
  运行期 `Err`(fail-closed)。句柄在构造期绑定该工具在这条通道上的 scope(v1 单 scope 约束下唯
  一);装饰器从**操作的 CtxOp + 句柄的 scope** 得到 cell,audit 记这个 cell(§6.5 第 2 层)。
- **audit 记 cell,不记 capability 名。** capability 是**准入期**词汇(决定哪些 cell 被授权,§5.3);
  effect 记录是**执行期**事实,记客观发生了哪个 cell。多 capability 同 cell(比如
  `inspect-workspace` 和 `read-content` 都落 `(Alloc, Read, workspace)`,§9.1 manifest 例)时,操作
  就记这个 cell,不歧义,也不需要工具自报归因到哪个 capability 名。这样无参 `workspace()` 也是确定
  的——通道方法定 `ExtImpact`/`CtxOp`,句柄定 `scope`/`trust`,cell 就完整了,跟 capability 无关。
  capability 名只用于准入配置(profile 里写"允许 inspect-workspace")和 LLM 投影,不进 effect 记录。
- **约束:一个工具在一条通道上只绑定一个 scope**(observe→`workspace/workspace`、
  fetch→`external-host/network`、tracer→`session/session` 全都自然满足);违反的注册校验报错。这条
  约束让"通道 + 方法 → 完整 cell"静态确定;真有多 scope 同通道的需求出现时,再引入在取用点选
  scope。
- **不再发放模式 B 设施句柄**(§6.2 superseded)。
- **audit/provenance 证据 sink** —— 只写。工具*提交*领域证据;它不决定要不要记账。
- **events** —— 结构化 `cep.*` 发射。
- **cancellation** —— 协作式取消 token。
- **identity** —— `turn_id`、`call_id`(及按需的 audit-context ids)。

`ToolHost` **不**暴露 `policy_hook`、原始 `budget`、`effect_ledger` 句柄、`authorized_region`、
`routing_invocation`、`cep_mode`。这些收进通道装饰器和 runtime(§6.5)。

### 6.4 通道供需与 fail-closed(不宿主兜底)

- 注册表里流动的只有 Tool 注册(manifest 生成的 host `inventory` / embedded Component);**通道实现
  从不进注册表**——它由 Direct-Local runtime 在宿主侧组装(`lancet-runtime` / `lancet-native` 的
  Local Workspace、network、session 通道)。
- 开 turn / 启动 composition 时匹配校验:工具的**派生通道集**(§6.3)⊆ 当前 runtime 供给的通道集,
  不满足 → 这个工具不进 catalog / dispatch 报 `Unavailable`。descriptor 不带独立 `channels`
  字段(派生量)。带 `hosted_backend` 的 descriptor 在 ADR 0004 下不得作为成功路径通过。
- **永不静默宿主兜底**(既有原则上升为规范条款):missing / failing backend 是错误,不是回落到
  未强制直发的理由。
- adapter / 测试差异对工具不可见:生产 `network` = Tool Host + `net-guard`(§6.6);CI 可用确定性
  测试实现。这是通道 seam 的本义,不是兼容债。

### 6.5 三层强制

强制在工具的*外围*和*下方*执行,绝不靠工具自己去调 helper。装饰器模式:每条通道一个强制装饰器,
里面持宿主侧通道实现(Direct-Local Local Workspace / network / session)。

1. **准入**(runtime,dispatch 前):**工具可调 ⟺ `descriptor.capabilities ∩ allowed_caps ≠ ∅`**
   (至少准入一个它声明的 capability,不是"全部都要"——§5.3 两段式);满足 `max_boundary`
   (deny-`ex` 的 turn 拒绝任何 `ex` cell 的 capability)。**同一个动作里完成 capability→通道绑定
   校验**(§5.3/§6.3):派生通道集 vs runtime 供给,通过就按"该工具在各通道上、被准入 capability
   覆盖到的 cell"预先绑定通道句柄。
2. **逐操作**(通道装饰器,执行中):每个方法夹一层"**单次 cell 授权**(这次操作产生的 cell 须有
   已准入 capability 根植,§5.3 两段式)+ 准入(`ObservedScope`:path/host 在 turn 边界内)+ 记
   effect 证据(**cell**,§6.3)";`ToolHost` 只发装饰后的句柄。**证据来源 = 通道调用参数**:workspace
   轴取 path 参数、网络轴取请求的 url 参数(host)——跟 workspace 轴对称。
   **audit 记 cell(操作 CtxOp + 句柄 scope + 参数 `ObservedScope`),不记 capability 名**(归因规则
   见 §6.3)。网络轴的**强制**走 `net-guard` + `[lancet.network].allowed_hosts`(URL 规范化 /
   DNS / redirect / SSRF,§6.6);强制跟记账是两回事,记账取参数,强制由 host 侧执行。**这一层
   不碰 budget**;`max_single_read_bytes` 这种 per-op 上限是 backend 的输出整形
   (mechanism),不进框架预算。未覆盖到的 cell 操作在这一层失败,不是整工具被拒(工具还能调其他
   已覆盖 cell 的操作)。
3. **事后**(runtime,dispatch 后):由 runtime 经 `measure()`(o200k_base 参考 tokenizer,§11.8 决议
   记录)**量返回 `Value` 的 token 数**(不信工具自报)→ 扣统一 context 预算(§5.1);并校验
   observed effects ⊆ 声明的天花板——即今天 `apply_effect_check`(`transport.rs:177`)的泛化。
   **该校验对越界(实际效果超出声明 cell)有效,对漏记(有效果但没声明)无效**。
   漏记的剩余防线在 ADR 0004 下收窄为:WASM Component 无 ambient I/O,只能调已声明的 Tool Host
   imports;若平台提供 host-function 调用日志,漏记可进一步在运行时发现(§6.7–§6.8)。
   **context 预算按"进 context 的返回值"算,不按 backend 逐次读的字节算**——读 10 MiB 只返回
   1 KiB 摘要的工具只花 1 KiB context。(今天扣费在 handler 侧、按 `bytes_returned`:
   `observation.rs:127`、`external.rs:82`——"工具自测自扣",本设计上移到 runtime。)

### 6.6 网络通道(宿主 Tool Host + net-guard)

**决议:`fetch.web` 字节从宿主侧 Tool Host 的 `network` 通道发出。**
强制面 = `net-guard` + `[lancet.network].allowed_hosts`(URL 规范化 / DNS pinning / redirect 检查 /
SSRF 防护),**不是**已删除的 turn-scoped workspace egress 网关,也不是 guest helper 发包路径。

**角色划分:**

- `net-guard` + `validate_fetch_url`(`lancet-authority/src/policy/network.rs`):CEP / 宿主侧预检
  与强制(第 1 道);`validate_fetch_url` 等领域 policy 随 #32 迁入 fetch 工具 crate,传输与
  allowlist 强制留在 Tool Host `network` 通道实现。
- **effect 记账取通道调用参数**(请求的 url/host),与 workspace 轴取 path 对称(§6.5 第 2 层)。
  强制跟记账分开:host 执行 allow/deny,记账取参数。
- **allowlist 唯一事实源**:`[lancet.network].allowed_hosts`。`[workspace].allowed_egress_*`
  已随 #62 删除,不再存在第二份 egress 表。
- **credential**:不把真值注入工具 ambient 环境;若需密钥,经宿主侧受控注入 / 占位机制(独立
  issue),effect 证据仍取通道调用参数。
- **定性区分:**#26 删的是 agent 面的自由命令执行;通道实现内部的固定传输 helper 是设施,不是
  agent 可编程的命令面。

### 6.7 承载面:WASM Component(ADR 0004 现行目标)

**逻辑规范**(cell/capability、通道契约、三层强制、供需校验)**跟平台无关,写死**。承载面以
ADR 0004 为准,不再把「进程内 trait + `inventory`」写成长期生产模型:

| | 过渡 / 生成层 | 现行目标(ADR 0004) |
| --- | --- | --- |
| 工具承载 | 早期讨论过的进程内 Rust crate | 每个 Tool 作为 WASM Component;`inventory` 仅作生成的 host 注册 |
| 通道 | trait 句柄(设计原型) | Tool Host WIT imports = 唯一存在的权威路径 |
| capability 声明 | 人手写 / generated descriptor | manifest 唯一 authority;build 生成 WIT / host metadata / registration |
| 模式 B | reserved/superseded 字段形状 | 禁止 hosted backend processes |

- 通道从「唯一提供的路径」变成「唯一存在的路径」:记账中介点、准入语义逐字不变。
- 通道 trait 形状设计**参考 wasi-filesystem / wasi-http 对应面**(设计约束,非依赖),为 WIT 平移留直路。
- **排除自研 effect-typed 语言路线**(WIT import 面 + 平台中介已覆盖其绝大部分收益;函数级 effect
  推断是研究级投资,不入主线)。工具语言 = 任何能编译到目标 wasm 的语言;默认工具作者面是 Rust。
- `inventory` **可以保留**,但语义是「嵌入 host 二进制的 generated Component 注册」,不是「任意
  native `&dyn Tool` 动态插件加载」。

### 6.8 过渡 ambient I/O 风险与关闭条件

在 native / trusted-plugin 过渡期,`ToolHost` **不是**完整沙箱:一个链接进 host 的 Rust crate
物理上仍可能调 `std::process::Command`/`reqwest`。边界曾靠 rev-锁定 crate 的源码审查 +
§13「处处无 ambient I/O」测试强制。

ADR 0004 关闭这条风险:**Component 无 ambient WASI filesystem / network / process**;唯一权威是
版本化 Tool Host imports。spec **不得**宣称过渡 native 路径已有结构性不可能;也**不得**把
trusted-plugin 写成 ADR 0004 之后的长期生产模型。

## 7. 结构性原则:agent 工具面 ≠ 系统 API

工具跟框架在**三根轴**上纠缠。PRD #24 点了前两根;audit 调查撞出了第三根:

1. **实现位置**(PRD problem #23)—— handler 编进框架 crate → 抽成工具 crate(#29–#32)。
2. **知识位置**(PRD problem #24)—— 工具 policy 在 `lancet-authority` → 搬进工具 crate(比如
   `observation.rs` → `lattice-tool-observe`,见 #30)。
3. **消费绑定**(本设计)—— 工具的能力*同时*是 operator/CLI 的 API。crate 拆分**修不了**这根:
   就算抽出 `lattice-tool-audit`,CLI 还是会把它当工具调。

处置:**`catalog` 的意思是"投影给 LLM",不是"可调用"。** operator 和程序化消费者(CLI、TUI)
直接调 store/backend **库**,而不是走 agent 工具面。`Tool` trait / Component export 只定义 agent 面。

承重实例是 audit(§10):`observe.audit`/`observe.audit_replay` 这个模型基本没人调,但同一批方法经
`lancet-api::{query_audit, tail_audit}` → `CepClient.call::<AuditObserve>` 撑着 `lattice audit`
CLI —— 起一整个 CEP 客户端、过策略准入、过事件总线,只为了读一个连它自己测试都直接开的 SQLite 库。
audit-read 是一个穿着 agent-tool 外衣的 operator 查询。

## 8. Substrate 类型(#27 骨架)

- `trait Tool: Send + Sync`(经 `#[async_trait]` async,对象安全;过渡 native / 测试面可用):
  `fn descriptor(&self) -> &'static ToolDescriptor`;
  `async fn call(&self, params: Value, host: ToolHost<'_>) -> Result<ToolOutput, CepError>`。
  ADR 0004 生产路径上,等价面是 WASM Component export + generated host 调度,而不是长期依赖
  native `&dyn Tool` 收集。
- `struct ToolOutput { value: Value, result_cell: Cell }` —— 工具报告本次调用的**返回值 Alloc
  cell**(§5.1:每次调用至少一个 Alloc cell)。runtime 校验 `result_cell` ∈ `descriptor.capabilities`
  覆盖的 cell 集(天花板校验)并自己 `measure(&value)` 扣 budget——**不是 descriptor 固定
  result_cell**,允许工具按 params 返回不同的 result_cell(比如 read_file 报
  `(Alloc, Read, workspace/in)`、todo_write 报 `(Alloc, None, session/in)`)。`value` 是进 context
  的返回值;`result_cell` 是这个返回值归到哪个 Alloc cell。
- `struct ToolDescriptor { name, capabilities: &[Capability] /* 每个 capability 根植唯一 cell,
  多个 capability 可共享同一个 cell(§5.3) */, scopes: &[ScopeDecl], hosted_backend: Option<HostedBackendDecl>,
  params_schema: fn() -> Value, description: &str, version: &str }` —— 编译期 / generated 事实源。
  **就顶替了旧 `EffectDecl`**(没有独立的 `EffectDecl` 类型;旧 catalog 的 `const EFFECT` →
  `DESCRIPTOR` / generated descriptor)。**新增 `description` + `version`**(投影 LLM tool definition 和满足 #35 的
  `list` 都需要)、**`scopes`**(`ScopeDecl { name, trust, channel }`,§5.2;`channel` = 模式 A 的
  `ChannelKind`——capability↔通道绑定链的锚点,工具的通道集由它派生,没有独立
  的 `channels` 字段)、**`hosted_backend`**(reserved / superseded;模式 B 已废止,§6.2;
  字段可留形状,不得激活)。无 `base_authority`(R9)。
- `enum ChannelKind { Workspace, Network, Session }` —— 框架封闭枚举,§6.2;纯数据类型,归
  `lancet-protocol`(§12)。
- `ToolHost<'_>` —— §6.3(构造期按 capability→通道绑定发通道句柄)。
- 生成的 `inventory` 注册(ADR 0004):build tooling 从 manifest 生成 `include_bytes!` / registration
  wrapper,嵌入 host 二进制。过渡期若仍有 native `inventory::submit!`,组合 binary 的 root 必须
  `use <tool_crate> as _;` 强制链接(或经聚合 crate `lattice-tools-default`)——见 §8.1;生产目标下
  registration 是显式生成产物,不再依赖链接器保留无引用静态项。
- `lattice-tool-prelude`(新,小)—— re-export 上述 + cell/capability/`in`-scope helper +
  Component / registration helper。
- `lancet-runtime`(新)—— 吸收 `lancet-dispatch`(registry + transport)与 `lancet-native` 的
  框架部分;拥有 Component 执行、生成 catalog 注册、Tool Host 强制、Local Workspace /
  network / session 通道(ADR 0004);**不依赖任何工具 crate**。彻底删除
  `lancet-native`/`lancet-cep`/`lancet-dispatch` 是 #34。

### 8.1 注册可达 + 动态 adapter + force-link / composition 契约(直修 major-revision item 2)

除非一个已注册的工具真能被 agent 触达,否则 substrate 是死的。本切片必须:

- 给 `MethodRegistry` / closed catalog 加一条 **raw / descriptor 驱动**路径,按 descriptor 接受
  Tool(今天的 `register_method<M: CatalogMethod>` 表达不了);
- 用 **descriptor 驱动**的 dispatch 替换 agent adapter 那个硬编码 8 臂 `dispatch_typed` match 和
  catalog 成员闸门(`cep.rs:110-121,151`),让名字不是编译期 catalog 常量的工具也能触达;
- 启动时把 generated / `inventory` 注册迭代进 registry,跟(过渡的)`register_native_*` 并存。

**过渡 force-link 契约**(仅当仍使用 native `inventory::submit!` 时):`inventory` 只收集**实际链接进
当前 binary** 的 submission——一个工具 crate 只是出现在 `Cargo.lock` / 作为依赖被解析了,但没有被
任何 `use ... as _;` 触及,链接器就可能丢弃它的 `inventory::submit!` 静态项(尤其 LTO / 无引用时),
runtime 就看不到它。这不是工具作者能控制的(他们写 `submit!` 没问题),是**组合层**的责任。本切片明确:

- **组合 binary 的 root crate**(`lattice` CLI / agent host)必须 `use` 每个期望启用的工具 crate
  来强制链接,等价于:

  ```rust
  use lattice_tool_observe as _;
  use lattice_tool_fetch as _;
  // ...每个启用的工具
  ```

  或提供一个**聚合 crate** `lattice-tools-default`(把全部一等公民工具 re-export 成 `_`),root 只
  要 `use lattice_tools_default as _;` 一行。两者择一,实现期定;**spec 在此写明组合层的这个义务**。
- **过渡期**:`register_native_*` handler 编进 `lancet-native`(同 binary),不经 `inventory`;
  迁移票(#29–#32)把各 handler 抽成工具 / Component 后,组合层补上对应的 generated registration。
- **release-gate 校验(不依赖链接器内部行为):** 链接器是否保留无引用静态项会随 LTO/codegen-unit/
  平台漂移,不能作测试基准。改成 **组合清单 == runtime 注册集** 的等价校验:维护一份
  `tools.enabled` 组合清单(profile/lockfile 级),release gate 断言注册返回的工具
  名集**恰好等于** 清单(不多不少)。多 → 有未声明工具混入;少 → force-link / embed 漏了。这个 gate 只
  断言两份可见事实相等。
- **ADR 0004 承载下 force-link 灰色地带消失**:Component 是显式嵌入 / 生成注册,不存在「依赖解析了
  但被链接器丢弃」;composition gate 校验 missing / duplicate / unapproved / descriptor-drift /
  import-drift(ADR 0004)。

## 9. manifest 格式 + drift 测试

`ToolDescriptor`(源 const / generated)是运行时的事实源。每个工具 crate / Source Tool 带一个
`manifest.toml`(name、version、声明的 cell/capability/scope、依赖),**机器镜像**这个 descriptor;
一个测试断言两者一致。**本切片定义 manifest 格式与 descriptor↔manifest drift 测试**,好让 #29–#32
能满足各自"manifest drift check 通过"的验收。`install/list/uninstall` CLI 与 release-gate 接线仍归
\#35。(这解掉 #29–#32 当前依赖一个排在它们下游 #35 才建的 drift 检查器的倒挂。)

### 9.1 manifest 结构(prototype,可演进)

下面这版按 **prototype** 级别定下 —— 够本切片与 #29–#32 跑通 drift check 即可,不上锁;真有多个
工具 manifest、或发现表达力不够时(比如 `params` 内联 JSON schema vs 文件 ref、capability 内联表
形态)再演进。格式 = TOML(PRD 点名 `manifest.toml`);放工具 crate / Source Tool **仓库根**;跟组合
binary 的 `Cargo.toml` 各管各的(rev-lock 依赖归 `Cargo.toml`,manifest 不重复)。

```toml
# manifest.toml — 工具 crate / Source Tool 仓库根;镜像 ToolDescriptor
name = "observe.workspace"
version = "0.1.0"
description = "Observe workspace content in four fixed modes: read_file / grep / list_dir / find_files."

[effect]
# fail-closed:未声明的 scope 视为 ex 且无物理落点(§5.2),引用它的 cell 注册即拒
[[effect.scopes]]
name = "workspace"
trust = "in"
channel = "workspace"              # 物理落点 = 模式 A workspace 通道(§6.3 绑定锚点)

# [hosted_backend]                 # reserved/superseded(§6.2);ADR 0004 下不得激活
# asset = "…"
# sha256 = "…"
# provision = "…"
# spawn = { argv = ["…"], ready_probe = "…" }

[[effect.capabilities]]                   # 一对多:一个 cell 派生多个 capability;cell.scope 引用上面声明的 scope
name = "inspect-workspace"
cell = { ctx_op = "alloc", ext_impact = "read", scope = "workspace" }

[[effect.capabilities]]
name = "read-content"
cell = { ctx_op = "alloc", ext_impact = "read", scope = "workspace" }

[params]
schema_ref = "src/params.rs"       # 或内联 JSON schema;drift check 比对 descriptor params_schema()
```

### 9.2 镜像关系

manifest 是 `ToolDescriptor` 的**镜像 + 少量超集**:

- **镜像部分**(drift check 比这些):`name` / `version` / `description` / `[[scopes]]`(含 `trust`+
  `channel`)/ `[[capabilities]]`(含 cell,cell.scope 引用已声明 scope)/ `[hosted_backend]`(若出现,
  仅作 reserved 形状比对,不得激活)/ params schema。**没有独立的 `channels` 字段**(派生自
  `[[scopes]].channel`)。
- **超集部分**(manifest 独有,不比):`dependencies`(rev-lock 归 `Cargo.toml`,manifest 可选带供人读)。
drift check = 解析 `manifest.toml` + 读 `ToolDescriptor`,逐字段比对镜像部分;不一致 → 测试失败。
**注册期一致性校验**(drift check 之外的运行时门):每个 `[[capabilities]]` 的 `cell.scope` 必须能在
`[[scopes]]` 里找到同名声明,否则注册报错(堵 scope 名拼错导致的"无物理落点"漏洞)。

> ADR 0004 承载下,`capabilities`/`scopes` 的事实源是 Source Tool manifest;build tooling 生成
> WIT / host descriptor / `inventory` registration。drift check 从「两份人手写声明互比」演进为
> 「manifest ↔ 生成产物 / 嵌入 artifact」比对。manifest 逻辑字段(scope/capability/cell)不变。

## 10. 跨票影响

这里的决策动了兄弟票的边界:

- **#27** —— agent scope 集排除 `audit`;从 #35 前移 manifest **格式** + drift **测试**;承载面以
  ADR 0004 WASM Component 为准(实现计划见 `docs/plans/2026-07-19-issue27-wasm-component-tool-substrate.md`)。
- **#29**(session tracer)—— 改 `Blocked by`,从 `#25`/`#26` 改成 **`#27`(substrate)/ `#28`
  (capability 准入)**;去掉不存在的 `#32a` 引用。host 侧 session store 在这票建(§6.2)。
- **#33**(audit)—— 从"把 audit 迁成 agent `Tool` crate"换形为"把 audit-read 抽成 **operator
  store 库**;不进 agent 面"。删掉作为 agent 工具的 `lattice-tool-audit`、`audit` scope、
  `(Alloc,Read,audit)` cell。`observe.audit`/`observe.audit_replay` 退出 catalog;
  `query_audit`/`tail_audit` 直接读 store(或一个 runtime audit service)。**已确认。**
  扫描证实 audit 是唯一有非 agent 消费者的催化方法(仅 `lancet-api::{query_audit,tail_audit}` →
  CLI);observe/mutate/fetch/session 没有任何 operator/程序化消费者,所以 #29–#32 无需换形。
- **#32**(fetch)—— 按 §6.6 切分:`validate_fetch_url` 等领域 policy(URL 规范化/scheme 白名单/
  response 整形)从 `lancet-authority/policy/network.rs` 迁入 fetch 工具 crate;**传输 +
  `net-guard` / `[lancet.network].allowed_hosts` 强制**进 Tool Host `network` 通道实现。
  `fetch.web` 字节走宿主侧 network 通道(已拍)。
- **#34**(删旧 crate)—— `lancet-runtime` 吸收边界清单(可证伪验收,按北极星定):
  + **进 runtime(框架 = 小而精数据流控制)**:dispatch registry + transport(吸收
    `lancet-dispatch`)、server 侧 audit lifecycle(`transport.rs:130/199`)、三层强制(§6.5:
    准入 / 通道装饰器 / 事后 `measure` + effect 校验)、events/cancellation、durable metadata、
    通道 trait + 强制装饰器 + 供需校验(`ChannelKind` 纯数据类型归 `lancet-protocol`,§12)、
    统一 context 预算与 `measure()` 模块(§11.8 决议记录)、Component 执行与 generated 注册
    (ADR 0004)。
  + **不进 runtime**:工具领域 policy(`observation.rs` → #30、`network.rs` → #32 工具 crate)、
    通道实现细节(留 Direct-Local Local Workspace / `net-guard` 等宿主模块)、audit 读库
    (operator 库,#33)、tokenizer 词表(随 `bpe-openai` 依赖编译进二进制,可换,§11.8 决议记录)。
  + #34 验收 = 上述"进"清单全部搬迁完成 + `lancet-native`/`lancet-cep`/`lancet-dispatch` 删掉后
    workspace 构建/测试全绿。
- **#35** —— manifest **格式**移到 #27;#35 保留 CLI + release gate。
- **#28/#30** —— 实质不受影响;#30 已声明正确的 `(alloc, read, workspace)` cell,也已计划把
  `observation.rs` 搬进工具 crate。
- **待开 / 已落地相关**(定稿后由用户决定是否另立票):
  1. **credential 受控注入**(跟 #27 正交):真值不进工具 ambient;`[lancet.network].allowed_hosts`
     已是唯一 allowlist 事实源(§6.6;`[workspace].allowed_egress_*` 已随 #62 删除)。
  2. **模式 B 不再落地**(§6.2 superseded)。若再引入 hosted backend processes,先写新 ADR。
  3. **WASM Component 工具平台** —— ADR 0004 已定为现行目标;#27 实现计划跟踪落地,不另作「遥远终局」。

## 11. 决议记录(原"待决";4 项待拍已于第四轮全部落定)

每条只记决议要点 + 指向正文;rationale 见所引章节。

1. **`ToolHost` 面** —— 按 capability 发通道句柄(`workspace()`/`network()`/`session()`),没有
   单一 `backend()`;`events`/`cancellation` 暴露,`budget` 藏在 facade 后,不含 audit query。见 §6.3。
2. **逐操作强制** —— 每条通道一个强制装饰器(不是单一 `EnforcingBackend`),不碰 budget。见 §6.5
   第 2 层。
3. **manifest 格式 + drift 入 #27** —— TOML prototype(`[[scopes]]` 含 `trust`+`channel`、
   `[hosted_backend]` reserved、`[[capabilities]]`),没有独立的 `channels` 字段;注册期校验
   cell.scope↔`[[scopes]]`。见 §9。
4. **audit 换形** —— 移出 agent 面,#33 重塑成 operator store 读库。见 §7、§10。
5. **`CtxOp::Free`** —— 本期只到枚举变体、没有 `ToolHost` 接缝(PRD R10);一次调用多 cell 表示
   见 §5.1。
6. **R8 `FailureSemantics`** —— 删整个枚举和 `EffectDecl.failure` 字段。见 §5.4。
7. **backend I/O 面** —— 维持继承的 9 方法,通道化成 `workspace` 通道。见 §6.2。
8. **budget 统一为单一 context 度量** —— runtime 事后量返回 `Value` 的 footprint,不信工具自报;
   度量点 §6.5 第 3 层。单位 = **o200k_base 参考 tokenizer,经 `bpe-openai` crate(github/rust-gems,
   纯 Rust,词表编译进二进制,count 线性最坏复杂度)**;`measure()` 先作模块,不单开 crate;真实
   token 由 provider `usage` 校准。
   〔2026-07-19 修订:原决议 = DeepSeek HF `tokenizer.json` 资产 + 离线加载。改选 `bpe-openai` +
   o200k_base 后无运行时资产:pin 坍缩为 `Cargo.lock`,原资产验收(revision+sha256、许可随资产、
   确定性离线加载、加载失败 fail-closed)全部作废——fail-closed 构造性成立。o200k 为 200k 多语
   词表,中文压缩率可接受;这把尺只是内部记账货币,绝对精度由 provider `usage` 回校承担。
   实现归属 #28(#27 不含 budget/measure)。〕见 §6.5 第 3 层。
9. **通道模型 + capability↔通道绑定;模式 B superseded** —— 绑定链
   `capability → cell → scope → channel`,通道集派生自 `[[scopes]]`、无独立字段;生产只认模式 A
   三条通道。原模式 B(域宿养 / hosted backend processes)与 ADR 0004 冲突,**标为 superseded**;
   `hosted_backend` 仅 reserved 形状。供需校验 fail-closed,永不宿主兜底。见 §5.2/§5.3/§6.1–§6.4。
10. **网络轴** —— `fetch.web` 走宿主 Tool Host `network` 通道;强制面 = `net-guard` +
    `[lancet.network].allowed_hosts`(URL/DNS/redirect/SSRF);effect 记账取通道调用参数(url/host),
    与 workspace path 对称;allowlist 唯一事实源为 `[lancet.network].allowed_hosts`
    (`[workspace].allowed_egress_*` 已随 #62 删除)。见 §6.6。
11. **承载面** —— 逻辑规范跟平台无关、写死;ADR 0004 已定 WASM Component 为现行目标(WIT import
    = capability/通道权威、host function = 通道);`inventory` 保留为 generated host 注册。
    过渡 native ambient I/O 风险由 Component 无 ambient WASI 关闭。排除自研 effect-typed 语言。
    见 §6.7–§6.8。
12. **`lancet-runtime` 吸收边界** —— 框架收 dispatch/transport、audit lifecycle、三层强制、
    events/cancellation、durable metadata、通道契约 + 供需校验、budget/`measure`、Component 执行;
    工具领域 policy、通道实现细节、audit 读库全部外置。清单见 §10 #34 条。

## 12. 验收(模型地基)

高层 criterion;具体测试缝见 §13。

- `lancet-protocol` 定义封闭核(`CtxOp`/`ExtImpact`)、`in`/`ex` 语义、`ScopeDecl`(名+`trust`+
  `channel`)、`Capability`、`Cell`、cell-key、`ChannelKind`、`ToolOutput`;`ImpactKind`/
  `EffectScopeKind`/`OverlayBehavior`/`FailureSemantics` 删除。`ToolDescriptor` 顶替旧 `EffectDecl`
  (没有独立的 `EffectDecl` 类型)。
- `CepMethod`/`MethodDescriptor` 去掉 `IMPACT`/`BASE_AUTHORITY`/`impact`。
- `Tool`/`ToolDescriptor`(带 `description`+`version`+`scopes`+ reserved `hosted_backend`;**无独立 `channels`**)/
  `ToolHost`(构造期按 capability→通道绑定发句柄)定义;generated `inventory` / Component 注册编译通过;
  raw / descriptor 驱动 adapter 存在。
- agent scope 集 = `{ workspace, session, external-host }`;无 `audit`(§5.6/§11.4 决议记录)。
- §13 全部测试缝通过;`cargo test -p lancet-protocol -p lancet-catalog -p lancet-runtime
  -p lattice-tool-prelude` 与 `cargo clippy --workspace --all-targets -- -D warnings` 通过。

## 13. 测试缝

- **effect 类型**:`effect_contract_tests` 更新到新模型 —— 代表性 cell + fail-closed `in`/`ex`
  (没声明 `in` 的 scope 当 `ex`;deny-`ex` 的 turn 拒绝它);一次调用多 cell 表示(§5.1)。
- **注册 → round-trip**:新 `lattice-api` 级测试 —— no-op 测试工具作 dev-dependency,经 generated /
  `inventory` 发现、经 `CepClient` 调用、经 `ToolHost` 触达 `workspace` 通道、断言 audit 行;测试
  binary 显式 force-link / embed(§8.1);在 `cep_round_trip.rs` / `TestFsBackend` 先例上扩展,
  改用注册路径而不是 `register_native_*`。
- **capability↔通道绑定 + 两段式授权**:声明了 `network` scope 的工具,准入它的 capability 后
  `ToolHost::network()` 可用;没声明 `network` scope 的工具调 `network()` 失败。**alias 单独授权
  成功**:同 cell 的两个 capability,只准入其一时工具可调且对应操作成功;**未覆盖 cell 失败**:带
  可选 `ex` capability 的工具,只授权 `in` 时 `ex` 操作在第 2 层失败(不是整工具被拒,工具仍可调
  `in` 操作)(§5.3 两段式、§6.5)。**audit 记 cell 不记 capability 名**(§6.3)。
- **`ToolOutput.result_cell`**:runtime 校验 `result_cell` ∈ descriptor 覆盖的 cell 集,并
  `measure(&value)` 扣 budget;descriptor 不固定 result_cell,工具按 params 报告(§8)。
- **注册期一致性**:cell.scope 引用了未声明 scope 名的 descriptor 注册报错。
- **force-link / composition / release-gate**:注册返回的工具名集**恰好等于** 组合清单 `tools.enabled`
  (§8.1)——多/少都失败。
- **drift**:`manifest.toml` 跟 descriptor 不一致 → 测试失败(镜像面含 `[[scopes]].channel` 和
  `[[capabilities]].cell.scope` 引用;没有独立的 `channels` 字段)。
- **通道门控 + 供需 fail-closed**:没声明对应 scope/capability 的工具取通道句柄失败;声明了 runtime
  不供给的通道的工具在该 turn dispatch 报 `Unavailable`(断言无宿主兜底路径,§6.4)。
- **处处无 exec / 无 ambient I/O**:`workspace` 通道 trait 无 `exec` 方法、`ToolHost` 触达不到命令
  执行、`exec.command_run` 不在 catalog(指 agent/通道面,#26 后已成立);ADR 0004 下 Component 无
  ambient WASI filesystem/network/process imports。
- **network 强制验收(#32 落地时,integration lane)**:`[lancet.network].allowed_hosts` allowlist
  生效;DNS / redirect / SSRF 防护生效;**effect 记账取参数(url/host),与 workspace path 对称**
  (§6.6)。
