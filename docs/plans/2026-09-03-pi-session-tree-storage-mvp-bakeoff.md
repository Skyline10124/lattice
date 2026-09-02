# Pi-style Session Tree 持久层 MVP Bake-off 实施计划（4 Agents）

## 0. 计划元数据

- **目标**：验证 LATTICE 是否应把当前全 Git 控制面迁移为更适合 Pi-style Session Tree 的持久模型，并在统一契约下比较四条实现路线。
- **代码基线**：`main@34dcdc16cfb2d4e614dda04f514e77b38a26ccea`
- **基线日期**：2026-09-03
- **语义参考**：`earendil-works/pi@e266507b606b9552fa277252644054afd4384b11` 的 Session Tree：entry 持有 `id + parentId`，Session 持有可移动 `leafId`；branch 只移动 leaf，下一次 append 成为该 leaf 的 child，旧路径不删除。
- **实验性质**：本计划只产出可比较 MVP、correctness/crash harness 和报告，不修改生产 Session/Turn/Invocation/WorkspaceHistory 行为，不修改 ADR 0005，也不提供迁移承诺。
- **前置顺序**：#187 先独立落地，确保当前 Git baseline 的 store/config/lock 测试没有已知 false-green。#188 在 bake-off 结论前不扩大实施，不为可能退役的 Git 表示继续搬运大批测试。
- **Agent 数量**：严格四个 lane：
  1. Agent 0：共同契约、oracle、runner、全 Git MVP baseline；
  2. Agent 1：SQLite-only；
  3. Agent 2：SQLite authority + Git immutable payload；
  4. Agent 3：redb-only。
- **明确排除**：本轮不做 Ladybug、Neo4j、TerminusDB、Dolt、PostgreSQL、Apache AGE 或任何远程/daemon 数据库候选。
- **选择原则**：hard-gate correctness 先行；通过者再比较复杂度、Btrfs 尾延迟、写放大、部署与可维护性。不得用一个任意加权总分掩盖取舍。

## 1. 要回答的架构问题

本轮不是“证明 SQLite 最好”，也不是“证明 Git 应该保留”。它必须回答：

1. Pi-style Session Tree 的核心语义能否由 storage-neutral 模型完整表达？
2. SessionHistory、PendingTurn 和 Invocation control state 放进同一事务域，能减少多少 durable boundaries、恢复分支和同步 I/O？
3. Session Turn payload 保留在 Git immutable object 中，是否带来足够的 inspectability、去重或导出价值，足以抵消双介质 publication protocol？
4. 纯 Rust embedded KV（redb）能否在当前多进程 CLI 模型中提供可接受的正确性、部署和性能？
5. 当前“全部 durable fact 都写成 Git object/ref”的物理模型，在简化到同一 v0 契约后，和三个候选相比处于什么位置？
6. 专用图数据库是否确有必要？本轮不实现图数据库；只有关系/KV 候选无法满足明确查询或规模要求时，才为下一轮提供证据。

最终报告只能得出以下类型的结论：

```text
选择某候选进入 production design
保留当前 all-Git，但明确需要修哪些放大
需要第二轮补一个有证据支持的候选
证据不足，不能决策
```

不得因为某个 Agent 先完成或代码最熟悉就提前指定赢家。

## 2. 冻结 Pi-style Session Tree v0 语义

### 2.1 v0 节点粒度：一个已关闭 Turn

Pi 可以把 message、model change、compaction、label 等都作为 entry。LATTICE 当前最稳定的恢复和审计边界是一个 closed Turn，因此 v0 定义：

```text
一个 Tree node = 一个已关闭 Turn
```

节点包含该 Turn 新增的 conversation delta：

- 一个 accepted user message；
- 完整 assistant message；
- 已关闭 ToolCall/ToolResult（如果有）；
- typed Turn outcome；
- payload digest；
- 可选最小统计信息。

v0 不允许从未关闭的 Tool batch、单个 ToolResult 或 provider partial stream 中间分叉。

### 2.2 v0 是树，不是一般 DAG

每个 Turn 恰有零或一个**context parent**：

```rust
context_parent_turn_id: Option<TurnId>
```

- `None` 表示 Session root 的第一个 Turn；
- append 只能引用同一 Session 中已经存在的 closed Turn；
- 一个节点不能修改 parent；
- 不实现 merge commit、多 parent 或 side parent；
- 当前 Git physical parents、PendingTurn journal parents 不进入公共契约。

如果 production 未来仍需要 merge/side-parent，它必须作为 v1 的显式需求重新评估；不能让当前 Git 表示提前污染 MVP。

### 2.3 active leaf 是唯一对话上下文选择器

每个 Session 持有：

```rust
active_leaf_turn_id: Option<TurnId>
```

核心操作：

```text
select_leaf(old_turn)
    active_leaf = old_turn

append_turn(new_turn)
    new_turn.parent = active_leaf
    active_leaf = new_turn
```

选择旧节点只移动 active leaf：

- 不删除原路径；
- 不修改旧节点；
- 不自动创建 named branch；
- 不自动复制 Session；
- 不自动回滚 workspace。

下一次 append 自然从所选节点长出新 child。

### 2.4 Tree 导航与 WorkspaceHistory 分离

`select_leaf` 只改变后续模型上下文。它不得：

- 修改 Local Workspace bytes；
- 移动 WorkspaceHistory；
- 执行 Rewind；
- 修改项目 Git；
- 推断目标 Turn 的 workspace snapshot。

需要恢复文件状态时，用户仍显式执行 Workspace Rewind。MVP 只记录 Turn 中已有的 workspace checkpoint 作为 opaque metadata，不解释它。

### 2.5 公共身份不暴露后端 locator

公共模型只使用：

```text
SessionId
TurnId
InvocationId
```

不得进入 storage-neutral API：

```text
Git OID
Git ref
SQLite rowid
redb internal key
filesystem inode
database sequence 作为业务身份
```

后端 locator 可以出现在 backend-private diagnostics 中，但 canonical snapshot 和 oracle comparison 必须只使用领域身份。

### 2.6 确定性顺序

同一 parent 的 children 按权威 `created_seq` 升序返回；`TurnId` 只作 tie-break，不依赖 wall clock。

```rust
created_seq: u64
```

`created_seq` 在 append 的 authority transaction 中分配，Session 内严格递增。它是展示/游标序号，不替代 `TurnId`。

## 3. Storage-neutral 数据模型

共同模型：

```rust
struct SessionRecord {
    session_id: SessionId,
    active_leaf_turn_id: Option<TurnId>,
    version: u64,
}

struct TurnNode {
    session_id: SessionId,
    turn_id: TurnId,
    context_parent_turn_id: Option<TurnId>,
    created_seq: u64,
    payload_digest: [u8; 32],
    payload: TurnPayload,
}

struct ActiveTurn {
    session_id: SessionId,
    turn_id: TurnId,
    expected_parent_turn_id: Option<TurnId>,
    stage: ActiveTurnStage,
    version: u64,
}

struct PlanRecord {
    turn_id: TurnId,
    model_step_ordinal: u32,
    bindings: Vec<InvocationBinding>,
}

struct InvocationRecord {
    invocation_id: InvocationId,
    turn_id: TurnId,
    state: InvocationState,
}
```

MVP 的 `TurnPayload` 使用固定、最小、provider-neutral shape。优先复用当前 `lattice-conversation` 的 typed message serde；不得为每个 backend 发明不同 payload。

所有候选必须共享：

- 同一个 Rust contract crate；
- 同一个 canonical JSON/digest helper；
- 同一个 error enum；
- 同一个 snapshot/export schema；
- 同一个 deterministic ID fixture；
- 同一个 operation trace。

## 4. 共同 API

### 4.1 Tree core

```rust
trait SessionTreeBackend {
    fn create_session(&self, session_id: SessionId) -> Result<SessionRecord>;

    fn append_turn(
        &self,
        session_id: SessionId,
        expected_leaf: Option<TurnId>,
        turn: NewTurn,
    ) -> Result<AppendOutcome>;

    fn select_leaf(
        &self,
        session_id: SessionId,
        expected_leaf: Option<TurnId>,
        target: Option<TurnId>,
    ) -> Result<SessionRecord>;

    fn get_tree(&self, session_id: SessionId) -> Result<SessionTreeSnapshot>;

    fn get_active_path(&self, session_id: SessionId) -> Result<Vec<TurnNode>>;

    fn build_context(&self, session_id: SessionId) -> Result<Vec<NativeMessage<BoundToolCall>>>;

    fn export_canonical(&self) -> Result<CanonicalStoreSnapshot>;
}
```

### 4.2 最小 control-plane trace

为了验证“同一事务域”是否真的降低复杂度，所有候选还实现：

```rust
fn begin_active_turn(...)
fn bind_plan(...)
fn record_dispatch_started(...)
fn close_invocation(...)
fn finalize_active_turn(...)
fn recover_active_turn(...)
```

这里的 MVP 不是复制当前十二阶段 PendingTurn。只保留比较所需的最小 durable facts：

```text
Accepted
Bound
Closing
```

- `begin_active_turn`：固定 TurnId、expected parent 和最小 payload basis；
- `bind_plan`：原子写 exact ordered bindings 和 Invocation Reserved rows；
- `record_dispatch_started`：持久化 may-have-occurred 边界；
- `close_invocation`：原子写 terminal + bounded result material + closure cursor；
- `finalize_active_turn`：原子插入 closed Turn、推进 active leaf、清除 active Turn；
- `recover_active_turn`：只闭合，不重新执行 Tool。

全 Git baseline 若不能跨 repository 原子完成，应显式暴露可恢复的中间状态；不得伪装成单事务。

### 4.3 统一进程协议

每个 backend 产出同形状 executable：

```text
session-tree-mvp-<backend> --store <DIR> serve
```

stdin / stdout 使用一行一对象 JSONL：

```json
{"request_id":1,"op":"create_session","session_id":"..."}
{"request_id":2,"op":"append_turn","session_id":"...","expected_leaf":null,"turn":{...}}
```

响应：

```json
{"request_id":2,"ok":true,"result":{...}}
```

或：

```json
{"request_id":2,"ok":false,"error":{"kind":"conflict","detail":"..."}}
```

约束：

- stdout 只能含 protocol JSONL；
- diagnostics 写 stderr；
- 每个 request 是一个逻辑 durability operation；
- runner 不链接 backend implementation；
- crash tests 可启动多个独立 backend process 指向同一 store；
- backend 不得给 harness 提供后门读取内部状态，只有 canonical export 可用于比较。

## 5. 共同错误语义

所有 backend 映射到同一个闭集：

```rust
enum MvpErrorKind {
    NotFound,
    Conflict,
    AlreadyApplied,
    Busy,
    InvalidInput,
    Corruption,
    Durability,
    Unsupported,
}
```

关键规则：

- 同一 `TurnId`、同一 parent、同一 payload digest：`AlreadyApplied` 或等价成功，不重复节点、不移动错误 leaf。
- 同一 `TurnId`、不同 parent 或 digest：`Conflict`。
- `expected_leaf` 与权威 leaf 不同：`Conflict`。
- foreign Session parent / select target：`InvalidInput` 或 typed `Conflict`，但四个 backend 必须统一。
- store 数据不满足内部不变量：`Corruption`，不能降级为 NotFound。
- 锁/数据库暂时不可获得：`Busy`；不得伪装成 CAS conflict。
- fsync/commit 失败：`Durability`；不得报告 success。

## 6. Hard-gate 不变量

任何候选只要违反下面任一项，就不能进入性能排名：

1. 已插入 Turn immutable。
2. 每个 Turn 最多一个 context parent。
3. parent 必须属于同一 Session。
4. active leaf 为 root 或同一 Session 中存在的 Turn。
5. `append_turn` 的节点插入与 leaf 推进是一个 authority publication。
6. `select_leaf` 只移动 leaf，不删除或修改节点。
7. 同 expected leaf 的并发 append 恰好一个赢家。
8. exact retry 不重复；divergent retry fail closed。
9. reopen 后只能观察 crash 前或 crash 后的 authority state，不能观察半节点。
10. canonical active path 严格为 root → leaf。
11. built context 只包含 active path 上的 Turn delta。
12. active Turn 存在时，`select_leaf` 被拒绝，除非先完成 deterministic recovery。
13. `finalize_active_turn` 成功后同时满足：
    - final Turn 可读；
    - leaf 指向 final Turn；
    - active Turn 不存在。
14. canonical export 可以在全新 oracle 中重建相同 Tree、leaf、active state 和 Invocation state。
15. 没有两个同时可写的 authority。projection/cache/diagnostic ref 必须可删除重建。
16. 多进程访问不会损坏数据；若候选需要 daemon 或全局独占进程，必须显式判为当前 CLI 部署不匹配，不能隐藏在 harness 中。

## 7. Canonical traces

所有 trace 由 Agent 0 固定并提交；实现 Agent 不得修改 expected output。

### T1：Pi-style branching

```text
create S

append T1   parent=root
append T2A  parent=T1
append T3A  parent=T2A

select T1
append T2B  parent=T1
```

期望：

```text
root
└── T1
    ├── T2A
    │   └── T3A
    └── T2B  ← active leaf
```

断言：

```text
tree exactly contains T1, T2A, T3A, T2B
children(T1) == [T2A, T2B]
T2A/T3A 未修改、未删除
active_path == [T1, T2B]
context == payload(T1) + payload(T2B)
```

随后：

```text
select T3A
active_path == [T1, T2A, T3A]
```

### T2：root branching

```text
select root
append ROOT_B
```

旧整棵树保留；active path 只包含 `ROOT_B`。

### T3：exact/divergent retry

- exact replay 同一 `TurnId`、parent、payload；
- 相同 `TurnId` 改一个 payload byte；
- 相同 `TurnId` 改 parent。

断言：第一种不增加 node，后两种 fail closed。

### T4：Session isolation

创建 S1/S2：

- S1 append 不得引用 S2 Turn；
- S1 select 不得选择 S2 Turn；
- 相同显示顺序或相同短 ID 不得串线。

### T5：同 leaf 并发 append

两个独立 process 同时从同一 `expected_leaf` append 不同 Turn：

```text
exactly one success
exactly one conflict
leaf == winner
loser node 不得成为已发布 Tree node
```

使用显式 barrier 同步起跑，不使用 sleep 猜测并发。

### T6：不同 Session 并发 writer

至少 8 个 process 对 8 个 Session 并发 append。断言无串 Session、无全局 corruption，并记录是否被单 writer 串行化。

### T7：active Turn 最小生命周期

```text
begin_active_turn(T4, expected_leaf=T3A)
bind_plan([I1, I2])
record_dispatch_started(I1)
close_invocation(I1, Succeeded)
close_invocation(I2, NotExecuted)
finalize_active_turn(T4)
```

最终断言：

- T4 成为 T3A child；
- leaf 为 T4；
- active Turn 消失；
- I1/I2 terminal facts 完整；
- reopen 后状态相同。

### T8：recovery

在 T7 每个 durable boundary 后 SIGKILL。reopen 后：

- 不重新 dispatch；
- 不生成新 InvocationId；
- recovery 重复执行幂等；
- 最终要么保持可诊断 pending，要么得到唯一 closed Turn；
- 不产生两个 T4 或两个 terminal。

### T9：canonical export

每个 backend 的 export 经过 key 排序和 canonical JSON 后，与 oracle snapshot byte-equal。backend-private locator 不进入 export。

## 8. Crash harness

### 8.1 禁止时间猜测

backend 在 logical barrier 命中时：

1. 向 runner 持有的专用 FD 写入 barrier record；
2. `SIGSTOP` 自身；
3. runner 收到 exact barrier 后 `SIGKILL` 整个 process group；
4. 新进程 reopen 并导出 canonical state。

禁止：

- `sleep` 等待“可能已经写到某一步”；
- 根据 elapsed time 推断 barrier；
- 仅杀一个线程；
- 把 process crash 描述成 hardware power-loss 证明。

### 8.2 共同 barrier

每个 authority publication 至少暴露：

```text
after_prepare_before_publish
after_publish_before_ack
```

含义：

- `after_prepare_before_publish`：所有可产生的临时/immutable payload 已写，但 authority commit 尚未完成；
- `after_publish_before_ack`：authority commit 已返回成功，但尚未向 client 回 success。

### 8.3 backend-specific barrier

全 Git baseline：

```text
after_invocation_repo_publish_before_session_repo_publish
```

SQLite + Git：

```text
after_git_object_durable_before_sqlite_publish
```

这些额外窗口不要求其他 backend伪造对应步骤；报告用于量化双介质/双 repository 的恢复成本。

## 9. Durability 对齐

性能比较必须使用相同目标语义：

```text
进程崩溃后 acknowledged operation 不丢失
未 acknowledged operation 只能呈现 before 或 after
```

候选配置：

- **全 Git baseline**：复用 `lattice-store` 当前 object-before-ref、ref/directory fsync 和 strict flock 语义。
- **SQLite-only**：WAL、`synchronous=FULL`、foreign keys enabled；不得用 `NORMAL` 或 `OFF` 作为主结果。
- **SQLite + Git**：Git object 必须在 SQLite authority transaction 前 durable；SQLite 使用和 SQLite-only 相同的 WAL/FULL 配置。
- **redb-only**：使用该版本提供的最高同步 durability/立即持久化选项；不得用 eventual durability 赢 benchmark。

SQLite checkpoint：

- 主 operation latency 测量中关闭隐式 auto-checkpoint，避免随机把维护工作记到某一个 request；
- 每轮结束必须执行显式 checkpoint，并单独记录耗时与写入量；
- 最终报告同时给出 operation-only 与 maintenance-amortized 结果；
- 不允许只报告 WAL append 而隐藏 checkpoint 成本。

## 10. 实验目录与所有权

建议新建一个**不加入 root workspace**的 nested workspace：

```text
experiments/session-tree-mvp/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── crates/
│   ├── contract/
│   ├── oracle/
│   ├── runner/
│   └── report/
├── backends/
│   ├── git-baseline/
│   ├── sqlite/
│   ├── sqlite-git/
│   └── redb/
├── traces/
│   ├── tree.jsonl
│   ├── retry.jsonl
│   ├── lifecycle.jsonl
│   └── crash.json
├── schemas/
│   ├── protocol.schema.json
│   ├── snapshot.schema.json
│   └── report.schema.json
└── reports/
    └── README.md
```

raw run bundles 写入：

```text
target/session-tree-mvp/<campaign-id>/
```

并保持 ignored。仓库只提交：

- code；
- schemas；
- deterministic traces；
- report summarizer；
- 一份小型人工复核过的示例 summary；
- 最终 architecture comparison。

不提交大型数据库、WAL、Git object directories、strace raw logs 或 benchmark bundles。

## 11. 四个 Agent lane

### Agent 0：共同契约、oracle、runner、全 Git MVP baseline

### 唯一所有权

```text
experiments/session-tree-mvp/Cargo.toml
experiments/session-tree-mvp/Cargo.lock
experiments/session-tree-mvp/README.md
experiments/session-tree-mvp/crates/**
experiments/session-tree-mvp/traces/**
experiments/session-tree-mvp/schemas/**
experiments/session-tree-mvp/backends/git-baseline/**
root .gitignore 中本实验的最小条目
```

### 任务

1. 实现 storage-neutral contract、typed errors、canonical snapshot。
2. 实现纯内存 oracle。
3. 固定 T1–T9 traces 与 expected snapshots。
4. 实现 process JSONL runner、multi-process barrier 和 SIGKILL harness。
5. 实现报告 schema、raw sample writer 和 summarizer。
6. 实现 all-Git MVP baseline：
   - Session tree/payload 使用一个 private Git repository；
   - Invocation/control facts 使用第二个 private Git repository；
   - 使用 `lattice-store` object/ref/CAS/fsync primitives；
   - 保留可恢复 saga，不伪造跨 repository 原子性。
7. 给三个实现 backend 建立可编译 stub 和 shared manifest entries，使后续 Agent 不必改共享文件。
8. 由 Agent 0 在共同 manifest 中完成候选依赖的实施期审查与 exact pin，记录 `Cargo.lock`；实现 Agent 不得自行漂移版本。
9. 输出基线 correctness 与一轮 diagnostic measurement，作为 runner 自测，不作为最终 verdict。

### 边界

- 不修改 `crates/lattice-session`、`lancet-audit`、`lattice-api` 生产代码。
- Git baseline 是同物理模型的最小实现，不宣称 byte-for-byte 等同当前产品。
- 不把 current `SessionGraphStore` trait复制为共同 contract。
- 不为候选提供 backend-specific expected output。

### Agent 0 完成门

只有以下全部通过后，Agent 1–3 才从同一 commit 分支：

```text
oracle unit tests
protocol/schema tests
T1–T5 against in-memory oracle
crash runner self-test
git-baseline T1–T9 correctness
四个 backend stub build
```

### Agent 1：SQLite-only

### 唯一所有权

```text
experiments/session-tree-mvp/backends/sqlite/**
```

### 目标布局

```text
control.sqlite3
├── sessions
├── turns
├── active_turns
├── plans
├── plan_bindings
├── invocations
└── optional append-only events
```

### 必须证明

- node insert + leaf update 在一个 transaction；
- exact/divergent retry 用 unique constraints 和事务检查实现；
- parent/leaf Session ownership 有数据库约束或同事务验证；
- T7 final Turn + leaf + active-turn close 同事务；
- WAL/FULL 下 common crash barriers 通过；
- 多 process reader/writer 行为可用；
- recursive path/tree 查询不需要把所有节点载入后再由应用层修复错误；
- canonical export 不暴露 rowid。

### 禁止

- 另建 Git payload store；
- SQLite 与 sidecar 双写；
- 用 `synchronous=NORMAL/OFF` 作为主报告；
- 把单进程 connection cache 当作多进程正确性证明；
- 为性能复制 materialized tree projection，除非它可删除重建且单独报告。

### Agent 2：SQLite authority + Git immutable payload

### 唯一所有权

```text
experiments/session-tree-mvp/backends/sqlite-git/**
```

### 目标布局

```text
control.sqlite3     # 唯一 authority
session-history.git # immutable Turn payload objects
```

SQLite 保存：

```text
Session/leaf/version
TurnId -> parent/created_seq/payload_digest/GitOid
active Turn
plan/invocation state
```

Git 保存：

```text
immutable Turn payload
```

### publication protocol

```text
1. 构造并写 Git immutable object；
2. 完成 object durability barrier；
3. SQLite transaction 校验 expected leaf；
4. 插入 TurnId -> GitOid mapping；
5. 推进 leaf / 关闭 active Turn；
6. COMMIT SQLite；
7. 返回 success。
```

SQLite commit 是唯一 publication authority。

### 必须证明

- SQLite 永不引用缺失或未 durable 的 Git object；
- crash 在 object durable、SQLite 未提交时只留下 orphan object；
- orphan object 不进入 Tree/context；
- crash 在 SQLite commit 后、ack 前 exact retry 幂等；
- Git refs 若存在，只能是可删除 projection；measured mode 默认不需要 ref；
- canonical export 从 SQLite authority 读取 mapping，再验证 Git payload digest；
- 不从 Git ref或 repository scan反推 Session leaf；
- 和 SQLite-only 相比，明确量化 inspectability/size/latency/复杂度差异。

### 禁止

- Git ref 与 SQLite row 同时决定 leaf；
- 两阶段提交协调器；
- background repair daemon；
- 为清理 orphan object提前实现 GC；
- 把“Git object 可读”当作已发布 Turn。

### Agent 3：redb-only

### 唯一所有权

```text
experiments/session-tree-mvp/backends/redb/**
```

### 目标布局

一个 redb database，至少包含：

```text
sessions
turns
children index
active_turns
plans
plan_bindings
invocations
events
```

### 必须证明

- 一个 write transaction 完成 node insert + leaf update；
- secondary index 与 primary rows 同事务更新；
- exact/divergent retry；
- T7 finalization 同事务；
- strongest durability 下 common crash barriers；
- process restart；
- 两个独立 process 的实际 open/read/write 行为；
- 若 engine 不能满足当前 CLI 多进程模型，必须返回明确 `Unsupported/Busy` 并在报告中判定需要 daemon 或外部全局 lock；
- 不用内存缓存弥补缺失的持久 index；
- canonical export 与 oracle byte-equal。

### 禁止

- 隐藏单 writer/单 process 限制；
- 在 runner 内串行化所有 redb request 而声称 backend 支持并发；
- 新建 daemon 来绕过 MVP 结论；
- 自己实现通用 SQL/graph query layer；
- 用 weaker durability 配置赢 benchmark。

## 12. Agent 协作与分支顺序

### Phase 0：#187

先完成并合并 #187。它不依赖本实验，且修正 baseline 相关的测试真实性。

### Phase 1：Agent 0 串行前置

建议 branch：

```text
experiment/session-tree-mvp-contract
```

Agent 0 完成 shared contract、runner、Git baseline 和 backend stubs。review 只检查共同语义与偏见：

- contract 是否带入 Git/SQL locator；
- trace 是否对某一候选特殊有利；
- oracle 是否独立于 backend；
- crash barrier 是否无 sleep；
- report 是否记录维护成本而不只记录速度。

### Phase 2：Agent 1–3 并行

三条 branch 全部从 Agent 0 同一 commit 创建：

```text
experiment/session-tree-mvp-sqlite
experiment/session-tree-mvp-sqlite-git
experiment/session-tree-mvp-redb
```

实施 Agent 不得修改：

```text
contract
oracle
traces
expected snapshots
report schema
其他 backend
```

若发现 contract 无法实现，提交一个最小 contract-change proposal，由 Agent 0/integrator统一修改并将相同变更 rebase 到所有 lane；不能在本 backend 私自扩协议。

### Phase 3：统一集成与 campaign

集成 branch：

```text
experiment/session-tree-mvp-bakeoff
```

只合并四条 lane，不同时修改生产代码。统一运行 correctness、crash 和 benchmark campaign，生成最终报告。

## 13. Workload

### W1：线性 Tree

```text
1 Session × 1,000 Turns
payload：1 KiB
```

测量 append、active path、cold reopen。

### W2：Pi-style 分叉 Tree

确定性生成 1,023 nodes，最大深度和 branching factor 固定。测量：

```text
select_leaf
append from old leaf
get_tree
get_active_path
build_context
```

### W3：多 Session

```text
100 Sessions × 100 Turns
```

测量 session lookup、prefix-free exact identity、不同 Session writer。

### W4：较大 Turn payload

```text
500 Turns × 64 KiB canonical payload
```

比较 payload write、空间占用和 Git object/DB page 放大。

### W5：同 leaf contention

100 轮两个独立 process 同时 append。每轮使用新 Session 或新 leaf，记录：

```text
winner/loser correctness
lock wait
busy/conflict 分类
p99/max
```

### W6：不同 Session concurrency

8 个 process、8 个 Session，各 append 250 Turns。观察：

- SQLite/redb 单 writer convoy；
- Git repository lock convoy；
- overall throughput；
- reader progress。

### W7：control lifecycle

重复 T7 200 次，固定两个 Invocation。记录一个 logical Turn 的：

```text
durable commits/transactions
fsync/fdatasync
bytes written
recovery-visible states
```

### W8：cold process

每次操作启动新 process：

```text
open
get_tree
get_active_path
append one Turn
exit
```

模拟当前 CLI，而不是只测一个长寿命 connection。

## 14. 测量指标

每个 operation：

```text
p50
p95
p99
max
success/conflict/busy 数量
```

系统指标：

```text
fsync/fdatasync/msync 次数
rename/lock syscall 次数
bytes written（数据库、WAL、Git objects）
最终 store size
maintenance/checkpoint size 与耗时
CPU user/system time
peak RSS
cold-open latency
```

实现指标：

```text
backend-specific Rust LOC
unsafe/FFI surface
direct dependencies
native build dependencies
schema/index 数量
custom recovery branch 数量
crash points 数量
需要的外部 process/daemon
backup/export/inspection 手段
```

报告还必须列出：

```text
durable authority 数量
一个 final Turn 涉及的物理 stores 数量
一个 final Turn 的 authority publications 数量
可观察 partial state 数量
```

## 15. Campaign 规则

主环境：

```text
维护机本地 Btrfs
同一实际 mount
release build
同一 rustc/toolchain
同一 payload bytes
同一 trace seed
```

tmpfs 只作为 CPU/serialization diagnostic，不参与最终存储选择。

每个 workload：

- 至少 5 个独立 fresh-store run；
- backend 顺序轮换，避免固定顺序吃到相同 host 状态；
- 保存全部 raw samples；
- 不因超阈值删除样本；
- 不挑选最绿 run；
- 记录 `/proc/pressure/io`、mount 信息和 campaign identity；
- executable digest、source commit、Cargo.lock digest 写入 report；
- 同一 run 期间不得重新 build 覆盖 binary。

raw artifact：

```text
target/session-tree-mvp/<campaign-id>/<backend>/<run-id>/
```

最终 summary 必须能从 raw samples deterministic 重建。

## 16. 验证命令

Agent 0 建立统一入口，最终命令形状：

```bash
cargo test \
  --manifest-path experiments/session-tree-mvp/Cargo.toml

cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  correctness --all-backends

cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  crash --all-backends

cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  benchmark \
  --backends git-baseline,sqlite,sqlite-git,redb \
  --output target/session-tree-mvp/<campaign-id>

cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-report -- \
  summarize target/session-tree-mvp/<campaign-id>
```

额外静态检查：

```bash
# 实验不能进入 production workspace graph
cargo metadata --no-deps --format-version 1 \
  | jq -e '
      [.packages[].manifest_path
       | contains("/experiments/session-tree-mvp/")]
      | any | not
    '

# backend 不得互相依赖
cargo tree \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-sqlite
```

具体 package 名可在 Agent 0 实施时统一，但所有后端必须只有一个公共 contract/oracle 方向，不能互相引用。

## 17. 最终比较方法

### 17.1 先淘汰 hard-gate 失败者

以下任一成立即标为不合格：

```text
Tree/oracle mismatch
crash 产生半发布 authority state
same-leaf 双赢家
exact retry 复制节点
foreign Session parent 被接受
canonical export 无法重建
acknowledged operation 在 process crash 后丢失
存在 Git/DB 双 authority
当前 CLI 多进程模型会损坏数据
```

### 17.2 不使用单一加权总分

对剩余候选给出 Pareto 表：

| 维度 | 关注点 |
|---|---|
| correctness | crash、CAS、idempotency、isolation |
| simplicity | LOC、recovery branches、stores、durable boundaries |
| latency | Btrfs p50/p99/max |
| amplification | sync syscalls、written bytes、space |
| deployment | embedded、daemon、FFI、multi-process |
| inspectability | canonical export、原生工具、人工诊断 |
| migration | 从当前数据模型迁移的难度 |
| future fit | compaction、label、fork、Tree UI 的扩展成本 |

### 17.3 决策优先级

通过 hard gates 后，默认优先级：

1. authority 和 recovery 模型更简单；
2. 当前本地多进程 CLI 无额外 daemon；
3. Btrfs p99/max 和同步放大更低；
4. canonical export、备份和人工诊断可靠；
5. 实现和迁移成本可控。

只有实际需求证明原生 Git inspectability 值得额外介质时，SQLite+Git 才应胜过 SQLite-only。只有 redb 在多进程和运维上无隐藏成本时，才把纯 Rust/性能优势计为胜出证据。

## 18. 最终交付物

```text
docs/plans/2026-09-03-pi-session-tree-storage-mvp-bakeoff.md
experiments/session-tree-mvp/**                  # 四候选与共同 harness
target/session-tree-mvp/**                       # ignored raw bundles
docs/plans/<date>-session-tree-mvp-comparison.md # 最终结论
```

comparison 文件必须包含：

- exact source commit；
- dependency/SQLite/redb/gix versions；
- machine、kernel、filesystem、mount options；
- 每条 hard gate 结果；
- 每项 workload 的完整 summary；
- fsync/write amplification；
- crash state table；
- backend-specific LOC/依赖/部署；
- 选中方案或明确的未决项；
- production migration 前必须新增的 ADR/issue 列表。

在 comparison 完成前，不修改 ADR 0005 的 accepted architecture，不删除当前生产 backend，不宣布 SQLite/redb/Git 任一候选获胜。

## 19. 明确非目标

本轮不实现：

- TUI `/tree` 或交互式选择器；
- branch summary；
- compaction；
- label/bookmark；
- fork/clone Session；
- mid-Turn branching；
- multi-parent merge；
- WorkspaceHistory 自动 rewind；
- production `SessionGraphStore` replacement；
- 通用 pluggable backend framework；
- 当前 state migration；
- GC/repack/orphan cleanup；
- remote sync；
- service/daemon；
- Ladybug 或其他 graph database；
- PostgreSQL；
- Apache AGE；
- 数据库级 HA、replication、network protocol。

## 20. 完成定义

### Agent 0

- [ ] contract 不含后端 locator。
- [ ] oracle 与 deterministic traces 固定。
- [ ] process JSONL protocol 可用。
- [ ] multi-process crash barrier 无 sleep。
- [ ] Git baseline 通过 T1–T9。
- [ ] Agent 1–3 stubs 和 shared manifests 已冻结。

### Agent 1

- [ ] SQLite-only 通过全部 hard gates。
- [ ] WAL/FULL 与 checkpoint 成本均记录。
- [ ] 多进程 contention 与 cold process 真实执行。

### Agent 2

- [ ] SQLite 是唯一 authority。
- [ ] Git object-before-SQLite publication crash window通过。
- [ ] orphan object 不可见。
- [ ] 无 authoritative Git ref。

### Agent 3

- [ ] redb-only 通过全部可支持 hard gates。
- [ ] strongest durability 记录。
- [ ] 多进程限制被真实暴露，没有 harness 串行化掩盖。

### Integrator

- [ ] 四候选来自同一 contract commit。
- [ ] correctness 与 crash campaign 全量执行。
- [ ] 至少 5 次独立 Btrfs run。
- [ ] raw samples 未筛选。
- [ ] comparison 文件从 raw report deterministic 生成。
- [ ] 没有在证据前修改 production architecture。
