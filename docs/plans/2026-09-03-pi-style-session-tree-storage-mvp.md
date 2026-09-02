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
- **计划文件规则**：本文件是本轮 MVP bake-off 的唯一权威计划；各 agent 不得另写一套不同的 contract 或 benchmark spec。

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

    fn build_context(&self, session_id: SessionId)
        -> Result<Vec<NativeMessage<BoundToolCall>>>;

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
{"request_id":2,"op":"append_turn","session_id":"...","expected_leaf":null,"turn":{}}
```

响应：

```json
{"request_id":2,"ok":true,"result":{}}
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
- foreign Session parent / select target：统一返回 `InvalidInput`。
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

这些额外窗口不要求其他 backend 伪造对应步骤；报告用于量化双介质/双 repository 的恢复成本。

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
- **redb-only**：使用实现时所选版本提供的最高同步 durability/立即持久化选项；不得用 eventual durability 赢 benchmark。

SQLite checkpoint：

- 主 operation latency 测量中关闭隐式 auto-checkpoint，避免随机把维护工作记到某一个 request；
- 每轮结束必须执行显式 checkpoint，并单独记录耗时与写入量；
- 最终报告同时给出 operation-only 与 maintenance-amortized 结果；
- 不允许只报告 WAL append 而隐藏 checkpoint 成本。

## 10. 实验目录与文件所有权

新建一个**不加入 root workspace**的 nested workspace：

```text
experiments/session-tree-mvp/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── crates/
│   ├── contract/
│   ├── oracle/
│   ├── runner/
│   └── crash-protocol/
├── backends/
│   ├── git-baseline/
│   ├── sqlite/
│   ├── sqlite-git/
│   └── redb/
├── traces/
│   ├── tree-branching.jsonl
│   ├── lifecycle.jsonl
│   └── expected/
├── schemas/
│   ├── protocol.schema.json
│   └── report.schema.json
└── reports/
    └── README.md
```

raw run bundles 写到：

```text
target/session-tree-mvp/<campaign-id>/
```

不提交大型 JSONL、strace、database、Git object store 或重复 benchmark bundle。仓库只提交：

- source；
- trace；
- schemas；
- runner；
- report summarizer；
- 一份小型示例结果；
- 最终 decision report。

Agent 0 必须在其他 agent 分支开始前预建全部目录、stub crate 和 nested workspace members。Agent 1–3 不修改共享 workspace manifest；确需依赖/feature 调整时，以单独小 diff 请求 Agent 0 合入，避免并行冲突。

## 11. 四个 Agent 的职责

### Agent 0：contract、oracle、runner、全 Git MVP baseline

唯一所有权：

```text
experiments/session-tree-mvp/Cargo.toml
experiments/session-tree-mvp/Cargo.lock
experiments/session-tree-mvp/README.md
experiments/session-tree-mvp/crates/**
experiments/session-tree-mvp/traces/**
experiments/session-tree-mvp/schemas/**
experiments/session-tree-mvp/backends/git-baseline/**
```

必须产出：

1. v0 typed contract 和 canonical export；
2. in-memory oracle；
3. JSONL process protocol；
4. trace runner；
5. exact start barrier 和 SIGSTOP/SIGKILL harness；
6. syscall/latency/disk-size collector；
7. 所有 backend stub；
8. all-Git MVP baseline。

all-Git baseline 目标是比较**物理机制和 failure shape**，不是复制整个 production saga。它应使用当前 `lattice-store` kernel，保持和当前架构相似的两个 authority repository：

```text
sessions.git
invocations.git
```

约束：

- Session Tree node/leaf 使用 Git object/ref；
- Invocation lifecycle 使用独立 Git ledger/ref；
- 不修改 production crate；
- 不调用真实 provider、Tool、MSB 或 WorkspaceHistory；
- cross-repository operation 明确按 saga 实现；
- 报告每个逻辑操作产生的 blob/tree/commit/ref transaction/fsync 数量；
- 不把该 MVP 数字冒充当前完整产品的绝对性能。

Agent 0 是其他 agent 的前置。contract/oracle/harness baseline commit 合并后，Agent 1–3 才从该 exact commit 分支。

### Agent 1：SQLite-only

唯一所有权：

```text
experiments/session-tree-mvp/backends/sqlite/**
```

物理形状：

```text
control.sqlite3
├── sessions
├── turns
├── active_turns
├── plans
├── plan_bindings
├── invocations
└── invocation_events
```

要求：

- 一个 SQLite database 是唯一 authority；
- tree parent、leaf、active Turn、Invocation 都在同一数据库；
- foreign key、unique constraint 和 conditional update 表达不变量；
- `append_turn` 在一个事务内插入 Turn 并 CAS leaf；
- `bind_plan` 在一个事务内写 plan、ordered bindings 和 Reserved rows；
- `close_invocation` 在一个事务内写 terminal/material/cursor；
- `finalize_active_turn` 在一个事务内插入 Turn、推进 leaf、删除 active Turn；
- WAL + FULL 为主配置；
- statement/transaction 不跨 provider 或 Tool execution；
- 不加 Git、sidecar authority、background service 或 generic ORM。

实现报告必须列出 schema、index、transaction boundaries、checkpoint policy 和 `PRAGMA` 的实际值。

### Agent 2：SQLite authority + Git immutable Turn payload

唯一所有权：

```text
experiments/session-tree-mvp/backends/sqlite-git/**
```

物理形状：

```text
control.sqlite3       # 唯一 authority
session-payloads.git  # immutable payload object store
```

publication protocol：

```text
1. 构造 canonical Turn payload
2. 写 Git immutable blob/tree/commit
3. 完成 Git object durability barrier
4. SQLite transaction：
   - 验证 expected leaf
   - 插入 TurnId -> GitOid mapping
   - 插入 context parent / created_seq
   - 推进 leaf
   - 清除 active Turn（finalize path）
5. COMMIT SQLite
6. 返回 success
```

约束：

- Git ref 不是产品 authority；
- measured path 默认不更新 diagnostic branch ref；
- SQLite row 不得引用未 durable 的 Git object；
- crash 在步骤 3 与 4 之间只允许留下 orphan Git object；
- orphan object 不得让产品读到未发布 Turn；
- canonical export 从 SQLite authority 取得节点，再验证 Git payload digest；
- 不做 SQLite/Git 双写同一 leaf 或 branch；
- 不建设 two-phase commit；
- 不为 orphan GC 做 production 方案，只统计和提供实验 cleanup。

该候选必须量化：Git inspectability/去重带来的实际收益，是否足以抵消额外代码、barrier、文件数和 crash 窗口。

### Agent 3：redb-only

唯一所有权：

```text
experiments/session-tree-mvp/backends/redb/**
```

物理形状：

```text
control.redb
├── sessions
├── turns
├── children index
├── active_turns
├── plans
├── invocations
└── events
```

要求：

- 一个 redb database 是唯一 authority；
- 使用一个 write transaction 完成每个共同 atomic group；
- children/active-path 所需 secondary index 由实现显式维护并在 export 时交叉验证；
- 使用最高同步 durability；
- 不以进程内单例绕过多进程测试；
- 不引入 daemon 来掩盖文件打开限制；
- 若 redb 在当前版本无法支持一个 writer 与其他 CLI process 的要求，必须返回明确 `Busy/Unsupported` 并在 product-fit gate 中记录，而不是跳过测试；
- 不手写关系数据库、query planner 或通用 graph abstraction。

实现报告必须列出：表/多值表布局、索引维护、schema version、open/lock 模型，以及多进程限制。

## 12. 并行工作规则

执行顺序：

```text
Phase 0：#187 落地
Phase 1：Agent 0 完成 contract/oracle/runner/stubs/git baseline
Phase 2：Agent 1、2、3 从同一 Agent 0 commit 并行
Phase 3：Agent 0 或独立 integrator 只做合并、统一运行和结果汇总
```

Agent 1–3 禁止：

- 修改 contract；
- 修改 expected traces；
- 降低 durability；
- 增加 backend-specific skip；
- 改 runner 让自己的 backend 通过；
- 在 stdout 输出 diagnostics；
- 读取其他 backend 私有文件；
- 把自己的内部 ID 放进 canonical export；
- 修改 product crates、ADR、AGENTS、FIX 或 FINDINGS。

若共同 contract 有真实缺陷：

1. 三个实现 lane 停在当前 commit；
2. Agent 0 提交最小 contract 修订；
3. oracle 和全部 expected trace 同步更新；
4. 所有 backend rebase 到同一新 contract commit；
5. 不接受某一个 backend 私有例外。

## 13. Workload 与测量

### 13.1 correctness workload

所有 T1–T9 每个 backend 必跑，不能标 ignored。

### 13.2 performance workload

统一 payload 和 deterministic IDs：

| Workload | 形状 |
|---|---|
| W1 | 1,000 个线性 Turn，1 KiB payload |
| W2 | 1,023 个多分支 Turn，约平衡 tree |
| W3 | 100 Session × 100 Turn |
| W4 | 500 个 Turn，64 KiB payload |
| W5 | 同一 leaf 双 writer 100 轮 |
| W6 | 8 Session、8 writer process 并发 |
| W7 | 冷进程 open → `get_tree` / `active_path` / `build_context` |
| W8 | 最小 lifecycle T7 连续 100 轮 |

所有 campaign：

- 使用同一开发机；
- primary filesystem 为维护机 Btrfs；
- store directories 位于同一 mount；
- 每个 backend 使用 fresh store；
- backend 执行顺序随机化或轮换；
- 至少 5 次独立 campaign；
- 不删除慢样本；
- 不根据 outcome rerun；
- tmpfs 仅作 CPU/serialization 诊断，不参与最终选择。

### 13.3 记录指标

每个 operation：

```text
p50 / p95 / p99 / max
success / conflict / busy / durability counts
```

系统指标：

```text
fsync / fdatasync / msync 次数
openat / rename / flock 或数据库锁调用
写入字节数
最终 store 大小
maintenance/checkpoint 大小与耗时
CPU time
peak RSS
冷启动时间
binary size
```

工程指标：

```text
backend implementation LOC
unsafe / FFI 面积
直接和传递依赖数量
schema/index 数量
crash recovery 分支数量
authority publication 数量
需要 daemon 与否
backup/export/inspection 方式
```

`strace` 至少覆盖：

```bash
strace -ff \
  -e trace=fsync,fdatasync,msync,openat,rename,renameat2,flock,fcntl \
  <backend command>
```

原始 trace 写入 ignored campaign bundle，不提交仓库。

## 14. 比较与决策规则

### 14.1 先淘汰 hard-gate 失败者

以下任一失败直接标记 `not-qualified`：

- canonical trace 与 oracle 不一致；
- concurrent append 出现零个或多个赢家；
- crash 后出现半节点、missing payload 或重复 terminal；
- exact retry 不幂等；
- cross-session parent/select 可成功；
- acknowledged state 在 process crash 后丢失；
- canonical export 无法重建；
- 存在 Git/DB 双 authority；
- 依靠弱化 durability 才通过；
- 多进程访问会无类型 corruption。

### 14.2 不使用单一总分

通过 hard gate 后，以 Pareto 表呈现：

```text
correctness / recovery complexity
Btrfs p99 / max
sync syscall 与 write amplification
代码与依赖复杂度
部署模型
inspectability / export
迁移难度
```

不得用任意权重把明显的 trade-off 压成一个“87.4 分”。

### 14.3 优先级顺序

当两个候选功能等价时，按以下顺序偏好：

1. 更少 authority 和 durable boundary；
2. 更少自定义 recovery/validation 代码；
3. 更自然的多进程 CLI 模型；
4. 更低 Btrfs p99/max 和写放大；
5. 更少部署/备份负担；
6. 最后才是额外 inspectability。

### 14.4 预先声明但不预设结果

最终报告应分别回答：

- SQLite-only 是否已经足够，使 SessionHistory 也无需 Git；
- SQLite+Git 是否提供可量化而非想象中的 Git 价值；
- redb 是否因多进程或索引维护成本失去优势；
- all-Git 是否只需修 barrier，还是物理模型本身不合适；
- 是否存在必须进入第二轮图数据库评估的查询缺口。

## 15. 交付物

Agent 0：

- contract/oracle/runner/crash harness；
- all-Git baseline；
- shared README；
- report schema；
- exact command list。

Agent 1–3 各自：

- backend executable；
- backend README；
- schema/layout；
- durability 配置；
- known limitations；
- full correctness output；
- raw campaign bundle 路径；
- 小型 summarized result。

最终 integrator：

```text
experiments/session-tree-mvp/reports/final-comparison.md
experiments/session-tree-mvp/reports/final-comparison.json
```

最终报告至少包含：

1. baseline 和依赖版本；
2. hard-gate verdict；
3. canonical state diff；
4. crash-window matrix；
5. performance distributions；
6. syscall/write amplification；
7. LOC/dependency/deployment table；
8. failure/recovery state count；
9. 未解决问题；
10. 推荐候选与不推荐理由；
11. production design 下一步；
12. 是否继续、改写或关闭 #188/#153 相关工作。

## 16. 验证命令

Agent 0 完成后：

```bash
cargo test \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-contract \
  -p session-tree-mvp-oracle \
  -p session-tree-mvp-runner \
  -p session-tree-mvp-git-baseline
```

全部 backend 合并后：

```bash
cargo test \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  --workspace

cargo clippy \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  --workspace --all-targets -- -D warnings

cargo fmt \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  --all -- --check
```

correctness：

```bash
cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  conformance --all-backends
```

crash：

```bash
cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  crash-matrix --all-backends
```

benchmark：

```bash
cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  benchmark --all-backends --filesystem btrfs
```

summarize：

```bash
cargo run --release \
  --manifest-path experiments/session-tree-mvp/Cargo.toml \
  -p session-tree-mvp-runner -- \
  summarize target/session-tree-mvp/<campaign-id>
```

命令名称可在 Agent 0 实现中做最小调整，但四个 backend 必须消费同一 runner 和同一 trace；不得各自提供不可比较的 benchmark 命令。

## 17. 明确非目标

- 不实现 Pi 的 TUI `/tree`。
- 不实现 message-level 或 ToolResult-level branch anchor。
- 不实现 branch summary、compaction、label、bookmark、fork、clone。
- 不实现 named branch、merge、rebase 或 Git-compatible CLI。
- 不自动 Rewind WorkspaceHistory。
- 不连接真实 provider、Tool、MSB、network 或 mutation。
- 不迁移现有用户 state。
- 不建立 production pluggable backend trait。
- 不修改 `SessionGraphStore` 生产 trait。
- 不更新 ADR 0005 为新决定。
- 不恢复旧 SQLite identity、numeric call/audit/evidence ID。
- 不实现 Ladybug 或任何专用图数据库。
- 不实现 PostgreSQL 或 Apache AGE。
- 不实现 daemon、remote sync、multi-host 或 distributed transaction。
- 不做 GC/repack/自动清理设计；只统计 orphan 和维护成本。
- 不把 MVP benchmark 变成 blocking CI gate。

## 18. 完成定义

只有全部满足，才算本轮 bake-off 完成：

- [ ] #187 已独立落地。
- [ ] Agent 0 的 contract、oracle、trace 和 harness 先完成并冻结。
- [ ] 四个 lane 都基于同一 contract commit。
- [ ] all-Git、SQLite-only、SQLite+Git、redb 四个 backend 可由同一 runner 启动。
- [ ] T1–T9 对全部 backend 执行，无 backend-specific skip。
- [ ] crash harness 使用显式 barrier，不用 sleep 推断。
- [ ] durability 设置等价且已记录。
- [ ] 至少五次独立 Btrfs campaign，无删样本或挑 bundle。
- [ ] raw bundles 位于 ignored 路径，仓库只保留小型 summary。
- [ ] 每个 backend 有 hard-gate verdict。
- [ ] final comparison 同时包含定量和工程复杂度证据。
- [ ] 最终推荐不依赖任意单一总分。
- [ ] 未修改生产 storage/backend 行为。
- [ ] 未实现或暗中引入 Ladybug、PostgreSQL、Apache AGE。
- [ ] 结论明确说明下一步是 production design、第二轮证据，还是保留当前实现。
