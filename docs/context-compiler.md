# Context Compiler & the Memory-Hierarchy Model

Status: DRAFT
Date: 2026-06-18
Depends on: [uninode-spec.md](./uninode-spec.md), [lattice-cep-sequence.mmd](./lattice-cep-sequence.mmd)

graph 存在的理由是补上朴素向量 RAG 给不出的那一路召回——架构关系、隐藏依赖、
时序耦合。它与语义检索**互补而非替代**：实测两者无一边倒，图法赢"架构/隐藏依赖"，
向量/BM25 赢"语义定位"，混合最优（见 [prior-art 调研](./uninode-prior-art.md)）。
本文从内存层级的视角描述 context compiler——它是 context 窗口的换页器。

---

## Core Idea: Virtual Context

compiler 的角色一句话钉死：**context 窗口的 MMU + 换页器**。

LLM 以为自己能看见整个 codebase，其实只有一个有限窗口 + 一个换页器，按需/预测地
把图换进换出。图就是 swap file。这是 virtual memory 的直接同构——**virtual context**。

> "LLM as OS / 上下文当虚拟内存"这一隐喻不是本设计独创，已有 MemGPT / Letta、
> AIOS 等先例。本设计的差异在两点：换页对**框架透明**（agent 不自管内存），
> 换入的是图投影出的**结构化子图**而非扁平文本页。详见 [prior-art 调研](./uninode-prior-art.md)。

### 四级层级

```
KV-cache         ≈ L1/L2（已处理、前缀不变就近乎免费复用）
context 窗口      ≈ RAM（LLM 推理时直接操作）
.uninode 当前态图 ≈ SSD（快、current state）
git log（双链）   ≈ 磁带（慢、归档历史）
```

compiler 管 RAM↔SSD，git commit 管 SSD↔磁带。

---

## The Compiler Pipeline

**总原则：context 编译 = 对 typed graph 的一次 query-time 投影。** 存储的图是底料，
编译出的 context 是针对本次 trigger 的临时子图，用完即弃。

关键解耦：**trigger 类型只影响"播种"，播种之后的扩散/排序/打包对所有 trigger 一致。**

```
Seed → Expand → Rank → Pack
```

### Stage 1 — Seed（trigger → seed nodes）

唯一与 trigger 耦合的部分，按类型分派：

| trigger | seed 提取 |
|---------|----------|
| 用户 NL 消息 | 显式引用 ∪ 词法/语义命中 ∪ 活跃集 ∪ 常驻 |
| 文件变更（watch） | 变更的 File 节点直接做种 |
| 测试失败 | 失败 Test 节点 + 它 Cover 的 Symbol + 栈帧引用节点 |
| CI/build 错误 | error 的 file:line → File/Symbol 节点（见 Diagnostic 边） |
| 定时/cron | config 声明的 scope |

NL 消息的四个种子源，初始相关度按精度递减：

1. **显式引用**——字面出现的路径/符号/报错，对 node `path`/`name` 精确+模糊匹配。最高精度。
2. **词法/语义命中**——非字面部分对 node 名/内容检索。广但糙。
3. **活跃集**——最近几个 turn 的 ops 涉及的节点。多 turn 连贯性（"那个测试"）靠它。
4. **常驻**——config 声明的永远在场节点（AGENTS.md、构建清单）。

播种高精度低召回——**结构召回靠 Stage 2 的图补回**。注意这不是"抛弃 embedding"：
语义/词法召回仍保留在播种里（上面第 2 路），图补的是 embedding 给不出的**结构性**
召回。两路互补，合起来才是完整召回。

### Stage 2 — Expand（图结构补召回）

从种子沿 typed edge 向外的**带权相关度扩散**（best-first 优先级前沿）。这是经典
**spreading activation**（Collins & Loftus 1975）/ **Personalized PageRank** 在 typed
graph 上的应用，不是新算法；典型图谱检索（如 HippoRAG）用 PPR 做的就是这一步。

> **实现可直接借 HippoRAG 的 PPR 引擎**（NeurIPS'24）：种子的初始相关度 = PPR 的
> personalization vector，只从种子注入概率、集中在相关邻域而不漫灌全图；我们的
> `RelatedTo` 弱边 ≈ HippoRAG 的 synonymy edge（embedding 最近邻桥接）。借的是 PPR
> 这个 expand 引擎，**不是**它的 OpenIE 建图——我们的节点来自 scanner。

> **edge weight = 相关度传导率**，∈[0,1]，可按方向不对称。

每条边传导率不同（code 域举例）：

| edge | 正向传导 | 含义 |
|------|---------|------|
| Call (A→B) | 高（callee） | 改 A 必须知道在调什么 |
| Call (反向) | 中（caller） | 影响面，理解 edit 次要 |
| Contain | 高双向 | 结构必然 |
| Cover | 中 | 测试即意图说明 |
| Read/Modify (ops 史) | 频次决定 | 时序耦合——常一起改的文件 |

扩散规则：种子带初始相关度 → 沿边传播，每跳 × 传导率 × 衰减 → 节点累加多路径
入流 → 相关度跌破阈值即自然边界（非固定深度）。

> **每跳贡献按源节点出度归一化（fan effect，ACT-R Anderson 1983；SYNAPSE Eq.2 是近期
> 实例）**：从节点 j 传出的激活 ÷ `deg_out(j)`。一个 import 一堆东西的高出度节点不会把
> 种子激活无差别灌给所有邻居——治的是"传播污染"。注意它惩罚**出度**，与 Stage 3 的 IDF
> （惩罚**入度/被引**）是两件正交的事，见下。扩散可迭代到稳定（SYNAPSE 实测 T=3 收敛）
> 而非单趟前沿。

这一步是图的全部价值：被多个种子经多条强边到达的节点累积高相关度，即使很远——
一个被到处用的核心类型，朴素相似度永远找不到，但结构上必然相关。

### Stage 3 — Rank（结构相关度 × 内在信号）

```
final = R × (1 + α·recency + β·frequency_idf)
```

- **R**——Stage 2 的结构相关度，主导项
- **recency**——近期被改/读的节点加权，随时间衰减
- **frequency_idf**——按**全局被引/入度**降权：被到处引用的高入度节点（god-object、
  ubiquitous util）不特异，反而降权——"近一切"等于"对本查询不特异"。图版 stopword，防核心
  文件淹没。这惩罚的是**入度**，和 Stage 2 的 fan effect（惩罚**出度**）正交互补，各治一种
  "高连接"病：fan effect 管"往外灌"，IDF 管"到处被引"。god-object 的病是高入度，fan effect
  够不着，必须靠 IDF。（SYNAPSE 没有 IDF 项，因为对话记忆图里没有"被引 500 次的核心 util"
  ——god-object 是代码图特有的，所以 IDF 是我们图必留的。）

可选 **lateral inhibition**（top-M 强节点竞争压制，SYNAPSE Eq.3）做稀疏化锐化——但带
**Cognitive Tunneling 护栏**：过度抑制会在简单/单跳查询上压掉次要事实，反不如全取（SYNAPSE
实测 β 偏大即掉点）。默认仅在多跳/歧义查询启用。

### Stage 4 — Pack（score → token budget）

带保真度降级的背包，**抽取优先而非先全量再 trim**：

- 每个节点按不同保真度入场，各有 token 成本：
  - File: 全文 / head+相关 span / 签名摘要 / 仅提及（path+一行）
  - Symbol: 全体 / 仅签名 / 签名+docstring
- 预算吃紧时**降保真度而非丢弃**——"仅提及"的相关文件对模型仍是有用信号。
- **覆盖约束**：先保证各种子的近邻都有代表，再深挖单点。
- 主种子（正在编辑）永远全保真。
- **KV-cache 排序**：打包顺序不只看相关度，还要按易变性排——稳定节点（pinned、核心
  类型）放前缀，query-specific 易变节点放尾部，保住 KV-cache 前缀不失效。

---

## Salience: Extraction over Truncation

context 的单位是**显著切片**，不是 artifact。报错输出几百行栈帧穿过 library，真正
信号就一行 error + 一个指回 `/workspace` 的 `file:line`。

**架构跃迁：别在编译时从 blob 里抽——在 exec 回来那一刻（ingest）就把输出解析成
结构化 diagnostic 节点/边，那行 error 变成一条指回代码图的边。**

```
传统:   Exec → 存 10KB stdout blob → 整块喂 / 头尾截断
本设计: Exec → 解析 → Diagnostic 节点{severity, code, msg(一行)}
                    └─ Diagnostic edge ─▶ File src/auth.rs:42
        raw 输出归档（git / 可丢），不进图热路径
```

两个后果：

1. **编译时几乎零成本**——显著信号已是节点，不必每次重 parse。
2. **错误自动点亮正确代码**——diagnostic edge 让被指向的 File 在下一次 compile 扩散里
   直接成高相关种子。形成闭环：exec 出错 → 错误成边 → 边把相关代码喂进 context →
   agent 修 → 再 exec。

**归 bundle 提供**——extractor 加入 bundle 契约，与 scanner 并列：

```
bundle 契约:
  ├ NodeKind 集合
  ├ EdgeKind 集合
  ├ scanner    (项目文件/事件 → 节点)
  └ extractor  (artifact + query → 显著切片 / 结构化诊断)
```

`rust` bundle 懂 cargo 报错，`pytest` bundle 懂 traceback。需新 EdgeKind `Diagnostic`
(Operation → File:line)。token 利用率从 **O(artifact)** 降到 **O(salient slice)**——
build log 上轻松 100:1。

未知工具（无 parser）保留 raw 归档，编译时按需头尾/grep 兜底。

---

## The Memory-Hierarchy Mapping

这个类比对其中一部分是承重的（有 prior art 的真机制兜底），对另一部分只是启发式
比喻——下面会逐条标注，不要把所有映射都当成已落地机制：

| 设计决定 | 内存模型 |
|---------|---------|
| `id = blake3(kind‖path)`，不含 content | id 是**地址**，content 是单元值。赋值不改地址 |
| Promote | **write-back**（脏页刷盘） |
| Promote REJECT | 丢弃脏页 |
| CEP 校验 | **写屏障 + 内存保护单元** |
| 保真度降级 | 压缩交换（compressed swap） |
| 沿边扩散 | 局部性预取 |
| "仅提及"档 | mmap + demand paging（映射未驻留，首访缺页） |
| Summary 节点 | zswap（降分辨率常驻） |

CEP 四边界与内存保护的映射，**两条是真机制、两条是类比**：

| CEP 边界 | 内存概念 | 性质 |
|---------|---------|------|
| authority | 页权限（actor 能否写这块） | **真机制**——对应 capability security（CHERI 等），可落地 |
| context | 段界/地址空间（待在分配段内） | **真机制**——对应 information flow control（Denning 1976），可落地 |
| audit | ECC/校验（写入完整性） | 类比——ECC 是硬件纠错，这里是溯源完整性，机制不同 |
| lifecycle | 缓存一致性协议（跨层状态转换合法性） | 类比——借缓存一致性的直觉，非同款协议 |

### page fault 贵得离谱

硬件缺页是微秒级透明暂停-加载-恢复。LLM 不能在 token 生成中途暂停换页——一次缺页
是**一整个推理 round-trip + 重新处理 token**。所以这个换页器必须比硬件预取器准得多：
硬件预取错浪费几纳秒，这里预取错浪费一个 turn。预取质量是生死线。

### 常驻不免费

RAM 里常驻跨指令免费；context 里常驻每 turn 都要重发/重算 token，除非 KV-cache 命中。
这就是 Stage 4 要按易变性排序的原因。

---

## The Reference Trace（学习型预取）

> ⚠ **这是整套设计相对 prior art 最新、也最未经验证的一点。** 把硬件预取范式
> （Markov / correlation 预取器、learned cache eviction）迁到"语义图边权"上，目前
> 没找到同款先例——既是最强原创点，也是最大技术风险。失败模式很明确：LLM 缺页 =
> 一整个推理 round-trip，**成本不对称极端**；坏 trace 会**自我强化**（语义版 cache
> pollution）；冷启动 / 分布漂移是该领域历史老大难。**当前设计缺"学偏后的纠偏 /
> 负反馈"机制。落代码前应先做小 spike：量化学习边权相对纯静态先验的实际提升、
> 误预取率，再决定投入。** 详见 [prior-art 调研](./uninode-prior-art.md)。

**ops 流本质是一条 memory reference trace——系统最值钱的东西。** 内存系统所有威力
都来自挖访问轨迹（预取、缓存、工作集估计）。

这回答了 edge weight 谁来定：

```
weight = 静态结构先验 (scanner 给) ⊕ 学习到的访问后验 (从 trace 训)
```

scanner 给初值（Call 边天生权重高），访问轨迹像训练 Markov/correlation 预取器一样训练
它："agent 碰 auth.rs 之后 80% 碰 session.rs" → session.rs 预取权重升。

推论：**compiler 越用越准。** 冷启动只有结构边；用熟了结构 + 学习到的访问模式。系统
会学会反复做的任务类型的工作集。新项目 compile 得比老项目差不是 bug，是没攒够轨迹。

轨迹的三级存储（还是那个层级）：

```
per-turn ops.jsonl  = WAL（热、每 turn 清）
边/节点上的运行统计  = 训练好的权重（温、持久、随流更新）
git 里的原始轨迹     = 冷、要从头重训才读
```

所以持久化模型精确是 **write-back cache + 预写日志**：图本体在 Promote 刷盘，
ops.jsonl(WAL) 持续追加，崩溃 = replay WAL，Promote = checkpoint。

---

## Working Set & Thrashing

工作集理论预测一个必然失败模式：若**本质工作集 > 窗口**，就颠簸——每 turn 缺页、
反复 re-read、agent 原地打转。

- **检测**：缺页率高 + 进度低
- **解法**（各对应一个已知概念）：
  - **分解任务** = 工作集分区 → 拆 subagent 不只为并行，是为让每块工作集塞进 RAM
  - **压缩** = 上 summary
  - **扩窗** = 加 budget
- **declared plan = 未来访问串**：agent 先声明计划，compiler 即拿到未来引用串近似，
  可做 Belady 式预取/置换（淘汰计划近期不碰的）。**规划与内存管理合一。** 未来访问串不必
  只靠 agent 声明：可像 **CodePlan**（微软, FSE'24）那样用 **change may-impact analysis**
  从依赖图**自动推导**——改了 X → 依赖分析算出还要碰 Y/Z → 预取 Y/Z，比等 agent 自觉声明
  更可靠。该 may-impact 同时落在 CEP 的 **context 边界**（改动的合法波及面）。

---

## Open Threads

- **Summary 节点**——把"压缩"变成图里一等公民。待定：谁生成（bundle extractor？单独
  LLM pass？）、何时失效（被压缩子树一变就脏）、能否分层（summary of summaries）。
- **学习边权的纠偏机制**——见上节风险：坏 trace 自我强化如何检测与回退？需要负反馈
  / 衰减 / 与静态先验的混合下限。spike 验证收益后再细化。

## 已落定的决定（本模型推导出的）

- compiler = MMU + 换页器；context = RAM，图 = swap
- 四阶段 Seed → Expand → Rank → Pack
- edge weight = 传导率 = 静态先验 ⊕ 学习后验
- 抽取优先；Diagnostic 边；bundle extractor 契约
- Stage 4 按相关度 + KV-cache 易变性双重排序
- ops.jsonl = WAL；运行统计持久在图上；原始轨迹在 git
- 任务分解 = 工作集分区；declared plan = Belady lookahead
