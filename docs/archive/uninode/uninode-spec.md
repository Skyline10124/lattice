# .uninode/ & NodeFile Spec

> **Archived, unimplemented draft.** Retained for research context only; do not
> treat this document as current architecture or an active implementation plan.

Status: DRAFT
Date: 2026-06-16
Depends on: [lattice-cep-sequence.mmd](./lattice-cep-sequence.mmd)

## Architecture

两层分离：agent 层和 uninode 基础设施层。

```
Agent 层:     view (local)      +  exec (sandbox)
uninode 层:   控制面 (host)      +  数据面 (sandbox)
```

### Host（控制面）

所有状态和逻辑在 host 侧：

- `.uninode/` graph 读写
- 从事件流构建 Operation 节点 + 边
- context compile（查图、排序、截取）
- CEP boundary validate
- promote / rollback（git commit / reset + snapshot）

### Sandbox（数据面）

sandbox 不持有图状态，不知道 `.uninode/` 的存在。接口只有两个：

| 方向 | 接口 | 内容 |
|------|------|------|
| host → sandbox | `exec(prog, args)` | 执行命令，返回 stdout/stderr/exit_code |
| sandbox → host | event stream | fanotify 事件流 (JSONL) |

### Agent 视角

agent 对 uninode 完全透明：

```
Agent: "读 main.rs"
  → Host: context compiler 查图，读 FS，返回内容

Agent: "执行 cargo test"
  → Host: 发 exec 到 sandbox
  → Sandbox: 执行，fanotify 上报事件
  → Host: 收事件，建 Operation + 边，更新图
  → Host: 返回 stdout/stderr 给 agent
```

---

## Core Principle

一切皆 node。NodeKind 和 EdgeKind 通过注册表扩展，不硬编码。
base bundle 提供最小完备集（File + Operation），domain bundle 按需叠加。

```
File₀ ──Read──▶ Op₁ ──Create──▶ File₁ ──Read──▶ Op₂ ──Modify──▶ File₁'
```

---

## NodeKind Registry

NodeKind 不是固定枚举，是 **bundle 注册表**。每个 bundle 注册一组 kind + edge kind +
对应的扫描器。注册表只规定扩展机制，不限定具体 kind。

消费者遇到不认识的 kind 或 edge kind 就跳过——前向兼容不破坏。

### base bundle（内置，始终可用）

| Kind | 语义 | 类比 |
|------|------|------|
| **File** | 内容/状态（任意类型：代码、文档、图片、配置、网页缓存） | Petri net place |
| **Operation** | 变换/动作（agent mutate、bash exec、web fetch） | Petri net transition |

File + Operation 构成完备的最小状态空间（Petri net place + transition）。这套
state/transition 二分不是新发明：对应 **W3C PROV** 的 Entity / Activity（边
Read/Create/Modify 对应 PROV 的 used / wasGeneratedBy），也对应 Petri net 的
place / transition。可直接拿 PROV 当理论锚点。详见 [prior-art 调研](./uninode-prior-art.md)。

Web 资源不需要独立 kind：fetch 动作 = `Operation { op_type: Fetch }`，
抓下来的内容 = `File { mime: "text/html", origin_url: "..." }`。

### domain bundle（可选，按需注册）

```
code bundle
├── NodeKind:  Symbol, Module, Test
├── EdgeKind:  Call, Import, Implement, Cover
└── Scanner:   语言感知的 AST 解析器

web bundle
├── NodeKind:  Page, Request
├── EdgeKind:  Link, Embed, Redirect
└── Scanner:   URL / DOM 解析器

custom bundle
└── 任意 kind / edge kind / scanner
```

### Bundle 职责

每个 bundle 提供：

| 职责 | 说明 |
|------|------|
| kind 集合 | 注册该 bundle 引入的 NodeKind 名称 |
| edge kind 集合 | 注册该 bundle 引入的 EdgeKind 名称 |
| scanner | 从项目文件 / 运行时事件生成对应节点 |

---

## Node 字段

每个节点在 JSONL 中是一个 JSON 对象。所有 kind 共享公共字段，
bundle 可添加 kind-specific 的扩展字段。消费者忽略不认识的字段。

### 公共字段（所有 kind）

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | string | blake3 复合哈希 `blake3(kind \|\| path)`，不含 content |
| `kind` | string | 注册表 tag: `"File"`, `"Operation"`, `"Symbol"`, ... |
| `bundle` | string | 来源 bundle: `"base"`, `"code"`, `"web"`, ... |
| `edges` | array | `[{ target, kind, weight }]` |

### File 字段（base bundle）

| 字段 | 说明 |
|------|------|
| `path` | 项目内相对路径 |
| `hash` | blake3 of raw content |
| `size` | 字节数 |
| `mime` | MIME 类型 |

### Operation 字段（base bundle）

| 字段 | 说明 |
|------|------|
| `op_type` | `Create` / `Modify` / `Delete` / `Exec` / `Fetch` |
| `command` | Exec 时的原始命令（可选） |
| `timestamp` | UNIX 时间戳 |

### 扩展字段示例（domain bundle）

code bundle 的 Symbol 节点会有 `name`、`parent`、`span` 等字段，
由 code bundle 的 scanner 生成，base 消费者直接忽略。

### Identity

`id = blake3(kind || path)`——不含 content。

id 在内容变更时保持稳定，边引用不会因为文件修改而断裂。
图是 current-state graph：节点可变（hash/size 等字段就地更新），历史交给 .uninode/ 的 git log。
不同路径的同内容文件仍然是不同节点（path 参与哈希）。

Operation 节点：`id = blake3("Operation" || op_type || command || timestamp)`。

---

## EdgeKind Registry

EdgeKind 同样是注册表，与 NodeKind 对称。base bundle 提供基础边类型，
domain bundle 按需扩展。Edge 的 `kind` 字段为 string tag，消费者跳过不认识的。

### base edges（内置）

```
Read        // Op 读了这个 File（输入，不变）
Create      // Op 创建了这个 File（输出，新）
Modify      // Op 修改了这个 File（输出，变更）
Delete      // Op 删除了这个 File（输出，移除/tombstone）
Contain     // 包含关系（目录含文件，File 含 Symbol）
RelatedTo   // 弱关联（tag/语义相似）
```

### domain edges（bundle 注册）

```
code bundle:  Call, Import, Implement, Cover
web bundle:   Link, Embed, Redirect
```

一条 bash 命令 = 一个 Operation 节点，多条不同 kind 的边：

```
Operation { op_type: Exec, command: "cargo build" }
  ├── Read    → src/main.rs
  ├── Read    → Cargo.toml
  ├── Create  → target/debug/app
  ├── Modify  → target/.fingerprint/...
  └── Delete  → target/debug/old_artifact
```

---

## Edge Detection

四种 EdgeKind 全部通过 fanotify 实时事件流捕获。
fanotify daemon 运行在 sandbox 内，事件流式回传到 host，host 实时构建 Operation 节点和边。

> 用 OS 级文件事件自动发现依赖是成熟技术，不是本设计独创：forward build system
> （rattle + fsatrace, OOPSLA'20）、eBPF build provenance、Bazel sandbox 都这么做。
> 新组合在于把这条事件流**喂进上下文编译 + CEP 审计**，而非用于构建增量。底层做法
> 应直接借现成经验（伪依赖判定、virtiofs 上的可靠性）。详见 [prior-art 调研](./uninode-prior-art.md)。

```
Sandbox                              Host
┌──────────────┐                    ┌─────────────────────┐
│ fanotify     │───event stream────▶│ edge builder         │
│ daemon       │  (JSONL via exec   │  → Operation node    │
│ /workspace   │   stdout stream)   │  → edges in graph    │
└──────────────┘                    │  → ops.jsonl append  │
                                    └─────────────────────┘
```

| fanotify 事件 | EdgeKind | 说明 |
|--------------|----------|------|
| `FAN_ACCESS` | Read | 文件被读取 |
| `FAN_CREATE` | Create | 文件被创建 |
| `FAN_MODIFY` | Modify | 文件被写入 |
| `FAN_DELETE` | Delete | 文件被删除 |

事件流格式（sandbox → host）：

```jsonl
{"edge":"Read","path":"/workspace/README.md","ts":1718556000}
{"edge":"Create","path":"/workspace/output.txt","ts":1718556001}
{"edge":"Modify","path":"/workspace/src/main.rs","ts":1718556002}
{"edge":"Delete","path":"/workspace/src/old.py","ts":1718556003}
```

ops.jsonl 在 host 侧实时追加，不依赖 sandbox 状态。sandbox 崩溃不丢已回传的事件。

### Edge 来源

| 操作来源 | 追踪方式 |
|---------|---------|
| Agent 显式读写 | host 侧 API 层直接记录 |
| Bash exec | sandbox fanotify daemon 流式上报 |

### 实测验证 (2026-06-16, microsandbox 0.5.6)

在 `docker.m.daocloud.io/library/python:3.12-slim` microVM (guest kernel 6.12.68) 中：

- `FAN_ACCESS` / `FAN_MODIFY`: 直接可用
- `FAN_CREATE` / `FAN_DELETE` (Linux 5.1+, `FAN_REPORT_DFID_NAME`): 验证通过
- guest 默认 CapEff=000001ffffffffff（全权限），fanotify 无需额外配置
- 性能开销接近零，纯 syscall，无额外包依赖
- fanotify 在 virtiofs 上的高并发场景（如 `cargo build`）未做压力测试，待验证

### 对比淘汰方案

| 方案 | 问题 | 状态 |
|------|------|------|
| strace (ptrace) | 10-50x 系统调用减速，需装包 | 淘汰 |
| FS diff (find + hash) | 非实时，有竞态，大仓库全量哈希慢 | 淘汰 |
| **fanotify 事件流** | **实时，零开销，零依赖** | **采用** |

---

## .uninode/ Directory Layout

按目录分文件（不按文件分文件），保留自然分级但不做 1:1 镜像。
每个 `_.jsonl` 包含该目录下所有直属节点（File、Symbol、Test...），
一行一个，按 `kind` 字段区分类型。

```
project-repo/
├── .gitignore              # 含 .uninode/
├── src/ ...                # 项目文件
└── .uninode/               # 独立 git 仓库
    ├── .git/
    ├── manifest.toml       # 图元数据（对齐的 project HEAD、schema version）
    ├── config.toml         # uninode 配置（注册的 bundle 列表等）
    ├── ops.jsonl            # Operation 节点（全局 append-only）
    └── nodes/              # 按目录分组的节点
        ├── _.jsonl          # 根目录文件节点
        ├── src/
        │   ├── _.jsonl      # src/ 直属文件 + 子节点（Symbol 等）
        │   └── utils/
        │       └── _.jsonl
        └── tests/
            └── _.jsonl
```

### _.jsonl 格式

```jsonl
{"id":"2f8a...","bundle":"base","kind":"File","path":"src/main.rs","mime":"text/x-rust","hash":"7b3e...","size":1024,"edges":[{"target":"9c4d...","kind":"Read","weight":1.0}]}
{"id":"b1c3...","bundle":"code","kind":"Symbol","name":"main","parent":"2f8a...","span":"1:1-10:1","edges":[{"target":"e4f2...","kind":"Call","weight":0.8}]}
{"id":"d7a9...","bundle":"code","kind":"Test","name":"test_main","parent":"2f8a...","span":"22:1-30:1","edges":[{"target":"b1c3...","kind":"Cover","weight":1.0}]}
```

- 按位置分组，不按类型分组——所有 kind 共存于同一个 `_.jsonl`
- 新增 bundle / kind 不改目录结构，只是多一种 `kind` 值
- 同目录内 rename = 改一行的 `path` 字段
- 跨目录 rename = 源 `_.jsonl` 删一行，目标 `_.jsonl` 加一行
- context compiler 查子树 = 递归读 `nodes/` 子目录的 `_.jsonl`

### ops.jsonl 生命周期

ops.jsonl 在 host 侧实时追加。Promote PASS 后清空（历史保留在 .uninode/ git log）。
每个 promote 周期只包含当前 turn 的 Operation。

```
Execution: fanotify 事件流 → host 追加 ops.jsonl
Promote PASS: git commit .uninode/ (含 ops.jsonl) → 清空 ops.jsonl → 下一周期
Promote REJECT: 截断 ops.jsonl 到上次 PASS 位置
```

### 三链同步 (project HEAD / .uninode/ HEAD / sandbox snapshot)

不是真原子操作。按重要性递减的顺序执行，每步失败有补偿：

```
Promote PASS:

  Step 1: git commit project repo
    失败 → 整体失败，什么都没变，重试即可

  Step 2: git commit .uninode/
    失败 → project 已推进，graph 落后
    补偿 → ops.jsonl 已在 host 磁盘，重试 git commit
           仍然失败 → 下次 Setup 时从 project 状态 reconcile graph
           （Operation 历史保留在 ops.jsonl 中，不丢失）

  Step 3: sandbox snapshot
    失败 → project + graph 已推进，snapshot 落后
    补偿 → 下次 turn 做 cold boot，一次性能损失，无数据丢失

Promote REJECT（终止 / 恢复梯子阶 3，见 Turn Lifecycle & CEP）:
  截断 ops.jsonl → git reset .uninode/ → restore last good snapshot
```

source of truth = project repo。.uninode/ 图可从 project 状态 reconcile，
sandbox snapshot 可 cold boot 重建。

详见 [lattice-cep-sequence.mmd](./lattice-cep-sequence.mmd)。

---

## Turn Lifecycle & CEP

一个 turn = 一个 user message → 一次 CEP promote cycle → 交付。turn 内可含多次
tool call，turn 末统一校验提交。前提：VM 已 warm 在 project HEAD（Setup 在上一
turn 末或会话开始时摊销，静止态 upper == lower）。

### 三阶段

```
Phase 1  Compile context (host, 本地)
  seed → expand(图遍历) → rank(weight × distance × recency × frequency)
       → select(token budget) → materialize(从 lower 读内容) → assemble

Phase 2  Agent loop (host ↔ LLM ↔ sandbox)
  read  → host 从同步视图返回（本地）；api 层记 Read edge
  edit  → 写 sandbox upper；直接生成 Modify/Create edge + Operation
  exec  → sandbox 跑命令；fanotify 流式回传 Read/Create/Modify/Delete
          host 建 Operation + edges，追加 ops.jsonl；stdout/stderr 回 LLM
  循环直到模型不再发 tool_call

Phase 3  Promote (host, CEP)
  collect ChangeSet → CEP validate → {PASS | REJECT → 恢复梯子}
```

读走本地、写走 sandbox：read 由 host 从同步视图直接返回（view=local），不绕
sandbox；edit/exec 落 sandbox upper（exec=sandbox），lower 在 turn 内保持干净。
host 视图双向同步：edit 主动下推、exec 改动经 fanotify 上送。

### 结束 token vs turn 边界

结束 token 是**模型**发的（`stop_reason: end_turn` / `finish_reason: stop`），框架
无法作者它，只能经 `StreamEvent::terminal` 观测。turn 边界是**框架**的抽象——模型
一次 end_turn ≠ turn 结束。框架在 end_turn 后接管跑 Promote，决定停 loop 交付
（PASS）还是继续（REJECT 当场 loop）。

### CEP 四边界

| 边界 | 校验问题 | prior-art 支撑 |
|------|---------|---------------|
| context | 改动是否在声明范围内 | **真机制**——information flow control（Denning 1976） |
| authority | agent 是否有权做这些操作 | **真机制**——capability security（CHERI 等） |
| audit | 一切是否可追溯（有果必有因） | 类比——借 ECC 直觉，非同款 |
| lifecycle | 状态转换是否合法 | 类比——借缓存一致性直觉，非同款 |

### REJECT 分类

| 边界 | 情形 | 可重试 |
|------|------|--------|
| Context | 改了范围外文件 / 意外位置创建 / 范围蔓延 / 读无关敏感路径 | ✓ |
| Authority | 触碰受保护路径 / 越权 / 违反 egress / 用未授权工具 | ✗ |
| Audit | 有果无因 / 事件流丢包 / hash 不符 | ✗ 系统故障 |
| Lifecycle | build/test 失败 | ✓ |
| Lifecycle | baseline 漂移 / 图不一致 / 三链无法一致推进 | ✗ |

### 恢复梯子（四阶）

```
阶 0  实时拦截（边界层）
     egress / 越界写当场被挡 → Phase 2 exec 失败 → agent 自我修正
     （大部分 Authority 违规死在这阶，到不了 Promote）

阶 1  agent loop 重试（可重试类）
     context 越界 / build-test 失败 → violations 回灌 → agent 改
     预算 = 3 次；保留 upper 不 reset（就地修违规，非推倒重来）

阶 2  框架自愈（无 agent 无人，确定性、有界）
     audit 缺口 / 丢包  → FS diff 兜底补齐缺失的边
     hash 不符         → 从 upper 重取重算
     baseline 漂移     → 自动 rebase（不冲突则三方合并）
     图不一致          → graph repair（剪悬空边、GC 孤儿）
     snapshot 失败     → 下个 turn cold boot
     → 自愈后重跑 CEP validate；过了就继续

阶 3  升级给用户（仅当阶 2 也搞不定）
     rebase 撞真冲突 / 重扫后改动本身违规 / 真完整性损坏
     / 绕过边界成功的 Authority 违规（绝不自动放行）
     → reset upper → 截断 ops.jsonl → 呈 violations 给人
```

要点：

- **预算只对阶 1 生效。** 阶 2 不烧预算（单次确定性尝试），阶 0/3 不涉及预算。
- **自愈有界**，不构成第二个无界循环；一击不中即上阶 3。
- **FS diff 在阶 2 复活**为 audit 修复兜底——fanotify 实时主路 + FS diff 离线兜底
  = 双机制冗余，比单 fanotify 更稳。淘汰的是它做主路，不是扔掉。
- **Authority 红线**：框架可**撤销**未授权效果，绝不**批准**；绕过边界成功的安全
  违规必上人，否则 CEP 安全保证破裂。
