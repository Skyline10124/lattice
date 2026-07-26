# uninode / Context Compiler — Prior Art & Related Work

> **Archived research.** This supported an unimplemented proposal and is not a
> current LATTICE architecture reference.

Status: DRAFT
Date: 2026-06-19
Depends on: [uninode-spec.md](./uninode-spec.md), [context-compiler.md](./context-compiler.md)

本文是对 uninode 执行模型 + context compiler 设计的外部 prior-art 检索。按设计的 10 个支柱
分节，每节给出**最接近的现有系统/论文/产品 + 与本设计的异同 + 新颖性判定 + 可借鉴点**。
最后一节是总体新颖性评估。

> 检索方法说明：本轮以 web 搜索为主（环境内 WebFetch 因证书校验问题不可用，无法逐页打开
> 验证原文），引用尽量落在 arxiv ID / 会议出处 / 官方仓库等可二次核验的锚点上。带 arxiv
> 2026 编号（26MM.xxxxx）的均为本轮搜索命中的真实条目（HuggingFace papers / alphaXiv /
> ACM / researchgate 多源交叉确认），但**未逐篇通读正文**，引用前请按需复核。经典文献
> （Denning、Belady、Kraska、MemGPT、PagedAttention、HippoRAG 等）为成熟已知工作。

---

## 支柱 1 — "一切皆 node"执行模型（File=place / Operation=transition）

**最接近的 prior art**

- **Petri nets**（Carl Adam Petri, 1962）。place/transition 二部图是并发系统的经典完备
  状态模型。本设计显式自比 Petri net，是直接的、正确的类比。
- **W3C PROV 数据模型**（W3C Recommendation, 2013，<https://www.w3.org/TR/prov-dm/>）。
  这是**最贴近的一个**：PROV 把世界建模为三类节点 **Entity / Activity / Agent** + 一组
  依赖边（`used`, `wasGeneratedBy`, `wasDerivedFrom`, `wasInformedBy`）。
  `Entity ≈ File`、`Activity ≈ Operation`、`used ≈ Read`、`wasGeneratedBy ≈ Create/Modify`。
- **数据血缘 / provenance graph**（"A Primer on Provenance", ACM Queue,
  <https://queue.acm.org/detail.cfm?id=2602651>；ProvLake 等）。retrospective provenance
  就是"节点是 data item 或 transformation，边是 data dependency"的有向图——与本设计的
  File/Operation/edge 同构。
- **Dataflow graph / build graph**（见支柱 4）：节点=文件，超边=命令的输入/输出集。

**相同**

- 用"状态节点 + 变换节点 + 有类型依赖边"作为最小完备建模，与 PROV / Petri net 同构。
- "一条命令 = 一个 transition 节点，多条不同类型的输入/输出边"——这正是 PROV 的
  Activity + used/generated 模式，也是 build graph 的多输入多输出超边。

**不同**

- **注册表式可扩展的 NodeKind/EdgeKind**：PROV 是固定本体（三类 + 固定关系，靠
  `prov:type`/扩展词汇延展），Petri net 只有两类。本设计把 kind 做成**开放注册表 + bundle
  契约（scanner/extractor）**，并要求"消费者跳过不认识的 kind"做前向兼容——这是工程层的
  扩展机制设计，PROV/Petri net 不提供。
- **current-state graph + id 不含 content**：PROV/血缘通常是 append-only 的历史 DAG；本设计
  是"current-state 可变图（节点就地更新）+ 历史交给 git log"，且 `id=blake3(kind‖path)`
  把 id 当稳定地址。这是务实的取舍，不是 PROV 的做法。

**新颖性判定**：执行模型本身**不是新发明**——它是 PROV / Petri net / 血缘图的重新表述。
真正的差异化在"**开放 kind 注册表 + bundle 契约 + current-state 可变图**"这套工程封装，
以及把它**接到 LLM context 编译**这件事（支柱 2/3）。建议：在文档里直接引 PROV 作为既有
理论锚点，省得每次都要从头论证模型的完备性。

**可借鉴**

- 直接复用 **W3C PROV 词汇**做边类型命名/对齐（`used`/`wasGeneratedBy`/`wasDerivedFrom`），
  能蹭到一套成熟语义和工具生态，也方便日后导出审计。
- Petri net 文献里的**可达性 / 不变量（place invariants）**分析，可作为 CEP `lifecycle`
  边界"状态转换是否合法"的形式化工具来源。

---

## 支柱 2 — typed graph 做上下文检索（而非朴素向量 RAG）

**最接近的 prior art**

- **aider repo map**（Paul Gauthier, 2023，<https://aider.chat/docs/repomap.html>；技术说明
  <https://aider.chat/2023/10/22/repomap.html>）。**这是最直接的对标**：tree-sitter 抽取
  符号 def/ref → 建符号引用图 → 跑 **(personalized) PageRank** 排名 → 按 token 预算选最重要
  的符号/文件喂进 context。开源复刻见 RepoMapper（<https://github.com/pdavis68/RepoMapper>）。
- **Sourcegraph（SCIP / code graph / Cody）**：持久化跨仓库的精确符号图（def/ref/implement），
  Cody 用它做检索增强。工业级"持久代码图投影出上下文"。
- **LocAgent: Graph-Guided LLM Agents for Code Localization**（arxiv 2503.09089）：把仓库建成
  图，graph-guided 定位要改的位置。
- **Code Graph Model (CGM)**（arxiv 2505.16901）：把代码图直接整合进 LLM 做 repo-level 任务。
- **Repository Intelligence Graph (RIG) / SPADE**（arxiv 2601.10112）：确定性抽取
  build/test 结构成图（components/runners/tests + dependency/coverage 边），作为 LLM 的
  "authoritative repo 描述"。和支柱 7 也强相关。
- **Codebase-Memory: Tree-Sitter-Based Knowledge Graphs for LLM Code Exploration via MCP**
  （arxiv 2603.27277）：tree-sitter 持久知识图，经 MCP 暴露给 agent，66 种语言。
- **GraphRAG**（微软, 2024）：通用域"先建实体关系图再检索"的代表作（非 code-specific）。

**相同**

- 核心命题完全一致：**持久 typed graph，查询时投影子图当上下文，优于把代码当扁平文本 embed**。
- 用图结构（而非纯相似度）补召回——"被很多东西引用的核心符号结构上必然相关，朴素相似度找
  不到"——这恰是 aider PageRank repo map 的立论。

**不同**

- aider repo map 是**无状态、每轮重算的快照**（基于当前 git 树跑一次 PageRank）；本设计是
  **持久、随 ops 流增量更新、带学习权重**的图（支柱 6）。这是本设计相对 aider 的主要增量。
- 多数现有系统**只建 code 图**；本设计的图是**统一基底**（File/Operation + 运行时 ops 边 +
  diagnostic 边 + web 等任意 bundle），exec 行为也进同一张图。范围更大。
- ranking 机制不同：aider/LocAgent 用 PageRank/图算法；本设计用**带权 best-first 扩散
  （spreading activation，见支柱 5）+ IDF 抑制 + recency/frequency**。

**新颖性判定**："typed graph 投影做 code context"这件事**已是拥挤赛道，且 aider 已经证明
有效**——本设计在这一层不是原创，应当**直接学 aider 的工程**而不是重造。真正的增量是
"**持久 + 增量 + 从访问轨迹学权重**"，以及把 exec/诊断/web 纳入同一张图。

**可借鉴**

- **直接照搬 aider 的 tree-sitter + PageRank repo map 作为 code bundle scanner 的冷启动基线**
  （它就是本设计的"静态结构先验"那一半）。算法、tag query、token 预算打包都能拿来用。
- Sourcegraph 的 **SCIP** 是成熟的跨语言符号图序列化格式，可作为 code bundle 的索引格式参考。
- RIG/SPADE 的"**从 build/test 元数据确定性抽取**"思路，正好填本设计 code bundle 里
  build/test 结构那块（CMake File API、CTest 那种确定性来源）。

---

## 支柱 3 — context compiler = context 窗口的 MMU / 换页器（"virtual context"）

**最接近的 prior art**

- **MemGPT / Letta**（Packer et al., "MemGPT: Towards LLMs as Operating Systems",
  arxiv 2310.08560）。**这是 virtual-context 类比的开山之作，也是最直接对标**：显式把 OS
  虚拟内存搬到 LLM——main context(RAM) / recall storage(disk) / archival storage(cold)，
  用 store/retrieve/summarize/update 原语在层级间换页，给 LLM "无限上下文"的错觉。
- **AIOS: LLM Agent Operating System**（arxiv 2403.16971）：把"LLM 当 OS 内核"做成系统，
  含 memory/storage manager 等 OS 风格子系统。
- **vLLM PagedAttention**（Kwon et al., SOSP 2023, <https://en.wikipedia.org/wiki/PagedAttention>）：
  把 OS 分页搬到 **KV-cache** 层——固定大小 block + 逻辑→物理块表，消除碎片。**注意这是
  不同的内存层**（本设计文档自己也点明了：KV-cache≈L1/L2，与 context-window≈RAM 不是一层）。
- 综述层面："A Survey of Context Engineering for LLMs"（arxiv 2507.13334）、"Memory for
  Autonomous LLM Agents"（arxiv 2603.07670）都把"hierarchical virtual context"列为一个标准
  机制族——说明这个范式**已被学界正式收编命名**。

**相同**

- "context 窗口=有限物理内存 + 一个换页器，把外部存储按需换进换出，给模型无限错觉"——这
  **正是 MemGPT 的中心论点**，一字不差。四级层级（KV/window/graph/git）的精神与 MemGPT 的
  分层一致。

**不同（也是本设计相对 MemGPT 真正的增量）**

- **换页器的换入单位与机制**：MemGPT 让 **LLM 自己调函数**（`archival_memory_search` 等）来
  换页——即换页决策在 agent 的 reasoning loop 里、由模型自己发起。本设计的换页器是
  **框架侧、对 agent 透明的**，且换入单位是 **typed graph 上的子图（沿边扩散）**，不是
  MemGPT 的 flat 文本/向量检索。即"**结构化预取 + 自动换页**" vs "**模型显式自检索**"。
- **预取**：MemGPT 基本是 demand paging（要了才取）；本设计强调**预测性预取**（支柱 6，从
  trace 学），并明确论证"LLM page fault = 一整个 round-trip，比硬件缺页贵几个数量级，所以
  预取质量是生死线"——这个**成本论证 MemGPT 没有展开**，是本设计的有价值视角。
- **类比的承重程度**：MemGPT 把虚拟内存当**叙事框架/灵感**；本设计把它当**承重的设计推导
  工具**（用内存模型反向验证 id=地址、Promote=write-back、CEP=写屏障+保护单元 等具体决定）。
  这种"把类比用到底、逐条对账"的做法，本身是少见的，且大概率能逼出更自洽的设计。

**新颖性判定**：核心隐喻**不是新的（MemGPT 已立旗）**，且"hierarchical virtual context"已是
被命名的机制族——**不要宣称发明了 virtual context**。但本设计把它**做成框架侧透明换页器 +
结构化子图换入 + 把类比当承重推导工具**，这套组合相对 MemGPT 有实质差异。

**可借鉴**

- 读 MemGPT/Letta 的工程实现：它在"何时触发换页、summary 如何生成/失效、层间一致性"上踩过
  的坑，正对应本设计的 Open Thread（Summary 节点）。**Letta 是现成可参照的参考实现**。
- vLLM 的 **prefix caching / 块表** 思想，对本设计 Stage 4"按易变性排序保 KV-cache 前缀"有
  直接工程价值——稳定前缀放前面，正是 prefix cache 命中的前提。

---

## 支柱 4 — 基于 fanotify 的实时边检测（FS 事件 → 依赖图）

**最接近的 prior art**

- **"Build Scripts with Perfect Dependencies"**（Sarah Spall, Neil Mitchell, Sam Tobin-Hochstadt,
  OOPSLA 2020, arxiv 2007.12737）+ **rattle**（<https://github.com/ndmitchell/rattle>）+
  **fsatrace**（<https://github.com/jacereda/fsatrace>）。**这是最直接的思想对标**：forward build
  system **通过追踪命令的真实文件访问来自动发现依赖**（而非手写依赖声明）。本设计的
  "exec → fanotify 观测真实读写 → 自动建边"在范式上**完全一致**。
- **eBPF-based build provenance**：Bomfather（"An eBPF-based Kernel-level Monitoring Framework
  ... Dependencies in Modern Software Supply Chains", arxiv 2503.02097）、
  <https://xieguochao.com/blog/projects/ebpf-for-dependency>。kernel 层在 `open()/read()` 上挂
  探针，实时收集每个被访问文件 + PID + 内容 hash，建依赖/provenance。
- **Bazel / hermetic build sandbox**：用 sandbox 强制并捕获声明 vs 真实文件访问，确保依赖完整。
- **AgentSight**（用 eBPF 给 AI agent 做系统可观测性，关联 LLM 行为与 kernel 事件）——把
  OS 级 trace 喂给/约束 LLM agent 的同代工作。
- 安全 provenance 老本行：ProPatrol（arxiv 1810.05711）等把 syscall 级文件事件建成 provenance
  图（攻击溯源）。

**相同**

- 用 **OS 级文件访问事件自动发现依赖**，免去人工声明——与 forward build system / eBPF build
  provenance **范式相同**。
- fanotify/eBPF/审计子系统作为低开销内核观测点，是这条线的标准工具集。

**不同**

- **消费目的不同**：build system 用 trace 做**增量/缓存/正确性**；安全 provenance 用它做
  **攻击溯源**；本设计用它**喂 LLM 上下文编译 + CEP 审计边界**。"FS 事件 → 边 → 进 LLM 上下文
  扩散"这个**下游用途**是新的组合（AgentSight 沾边，但它做的是约束/可观测，不是建图喂 context
  编译）。
- **fanotify 选型 + microVM 内运行**：本设计在 microVM guest 内跑 fanotify daemon、JSONL 流式
  回 host，且做了具体实测（FAN_CREATE/DELETE 需 Linux 5.1+ `FAN_REPORT_DFID_NAME`、virtiofs
  高并发未压测）。这种"microVM+fanotify+流式建图"的具体工程栈，没找到现成同款。
- 关于 fanotify 只给 PID 不给完整 credentials 的已知坑：本设计目前靠"host 侧 API 层 + sandbox
  fanotify"两路，没碰到 credential 归因问题——但若日后要细分 actor，这是已知雷区。

**新颖性判定**："用 OS 文件事件自动建依赖图"**绝非新发明**——build system 圈做了几十年，eBPF
provenance 是当前热点。但"**实时 FS 事件流 → typed graph 边 → 直接驱动 LLM context 编译 +
作为 CEP audit 边界**"这个端到端组合是新颖的。底层观测技术该直接学现成的。

**可借鉴**

- **直接读 rattle/fsatrace 与 Bazel sandbox 的工程经验**：他们最懂"哪些访问算依赖、如何处理
  伪依赖/临时文件/并发竞态、virtiofs/overlay 下的事件可靠性"。本设计 Open 的"virtiofs 高并发
  fanotify 压测"风险，他们的 issue tracker 里大概率有前车之鉴。
- **eBPF 作为 fanotify 的潜在升级路径**：若 fanotify 在归因/性能上撞墙，eBPF（如 Bomfather）能
  给到 PID + 内容 hash + 更细的 syscall 语义；本设计阶 2 自愈里"hash 不符 → 重取重算"也能从
  eBPF build provenance 的"每文件 SHA-256"做法直接借鉴。
- 本设计已做的"fanotify vs strace vs FS diff"淘汰对比是对的；可补一条：**Bazel/rattle 早就
  得出"sandbox + trace 优于 ptrace"**，与本设计结论一致，可引为佐证。

---

## 支柱 5 — Seed→Expand→Rank→Pack（图上带权相关度扩散）

**最接近的 prior art**

- **Spreading activation**（认知科学：Quillian 1968；Collins & Loftus, "A Spreading-Activation
  Theory of Semantic Processing", Psychological Review, 1975）。IR 综述：**Crestani, "Application
  of Spreading Activation Techniques in Information Retrieval", AI Review, 1997**。本设计 Stage 2
  的"种子带初始相关度 → 沿边 ×传导率×衰减 → 多路径累加 → 跌破阈值即边界"**就是教科书式的
  constrained spreading activation**，连"衰减 + 阈值 + fan-out 约束"都对得上。
- **Personalized PageRank (PPR)** 检索：以种子为 restart 分布在图上扩散。**HippoRAG**
  （Gutiérrez et al., NeurIPS 2024, arxiv 2405.14831, <https://github.com/OSU-NLP-Group/HippoRAG>）
  是当下代表：LLM+KG+PPR 模拟海马体索引，多跳 QA 比 SOTA 高 ~20%、比迭代检索便宜 10-20×。
- **SYNAPSE**（arxiv 2601.02744, 2026-01；已精读原文核实）：把 agent memory 建成动态图，相关度由
  **spreading activation** 涌现而非预存链接，迭代 Propagation→Inhibition→Sigmoid（T=3 收敛），
  三路混合检索（embedding + 激活遍历）。**和本设计 Stage 2+3 高度同构**。**更正一处 subagent
  误判**：它的 **fan effect**（Eq.2，激活÷出度 `deg_out`，引 ACT-R 1983）才是对应本设计 IDF 抑制
  的机制；**lateral inhibition**（Eq.3，top-M 强节点竞争压制）是另一件正交的事。temporal decay
  对应本设计的 recency。
- aider 的 PageRank repo map（见支柱 2）也是这一族（PPR 是 spreading activation 的特例）。

**相同**

- Stage 2 的扩散机制 = constrained spreading activation，**与 1975/1997 的经典定义逐条吻合**。
- "被多个种子经多条强边到达者累积高相关度（即使远）"——这正是 spreading activation / PPR 的
  核心收益论述。
- Stage 3 的 **IDF 式抑制高度节点**（god-object 降权）对应 SYNAPSE 的 **fan effect**（Eq.2，
  激活按出度 `deg_out` 归一化，引 ACT-R 1983），**不是** lateral inhibition——后者（Eq.3）是
  top-M 竞争压制，正交机制。但 fan effect 惩罚**出度**、IDF 惩罚**入度/被引**，对有向图是两件事：
  god-object 的病是高入度，须靠 IDF，fan effect 够不着（见下"可借鉴"的修正）。

**新颖性判定**：检索算法本体（spreading activation / PPR）**是 50 年的成熟技术，且 HippoRAG/
SYNAPSE 已在 LLM 检索场景落地**——本设计这一层是**经典算法的应用，不是原创**。应坦诚以
spreading activation / PPR 命名之，避免"重新发明"。

**可借鉴**

- **直接采用 PPR / spreading activation 的成熟实现与收敛/剪枝技巧**（Crestani 综述里有完整的
  decay/threshold/fan-out 约束工程经验；"approximate spreading activation"那条线专门解决大图
  上的效率）。
- **HippoRAG 的 PPR 工程**几乎可直接迁移到本设计 Stage 2（它就是"种子 → PPR → 取高分节点"）。
- **SYNAPSE 三机制各对应本设计不同部件（精读修正）**：① **fan effect**（÷出度）→ 补进 Stage 2
  扩散，治高出度"传播污染"；② **IDF 抑制**（÷被引/入度）留在 Stage 3 治 god-object，二者正交、
  fan effect 替代不了 IDF；③ **lateral inhibition**（top-M 竞争）可选做 Stage 3 稀疏化，但带
  **Cognitive Tunneling 护栏**（SYNAPSE 实测 β 偏大会在简单查询掉点，反不如全取）；④ **temporal
  decay** → 对应 recency 衰减。原文默认参数可作校准起点：S=0.8, β=0.15, ρ=0.01, M=7, T=3。
- Stage 4 的"保真度降级背包"更像本设计自己的工程贡献（见总体评估），可保留为差异点。

---

## 支柱 6 — 从访问轨迹学习边权重（ops 流 = reference trace，像训练硬件 prefetcher）

**最接近的 prior art**

- **硬件预取器**：
  - **Markov prefetcher**（Joseph & Grunwald, ISCA 1997）：从历史 miss 序列学"地址 A 后常跟
    哪些地址"的转移概率——**与本设计"碰 auth.rs 后 80% 碰 session.rs → 升权"逐字对应**。
  - **Correlation prefetcher / GHB**（Nesbit & Smith 2004）等：学不规则但重复的访问关联。
- **学习型缓存置换**：
  - **Hawkeye**（Jain & Lin, ISCA/HPCA 2016）：用 Belady oracle 标签训二分类（cache-friendly
    vs averse）。
  - **Glider**（MICRO 2019）：更好特征 + 更强预测器，miss rate 优于 Hawkeye。
  - **PARROT**（Liu et al., "An Imitation Learning Approach for Cache Replacement", ICML 2020）：
    把置换当**模仿学习**，模仿 Belady MIN。
  - **Pythia**（Bera et al., MICRO 2021, arxiv 2109.12021）：在线 RL 预取框架。
- **learned index / learned data structures**：Kraska et al., "The Case for Learned Index
  Structures", SIGMOD 2018（arxiv 1712.01208）——"索引/数据结构本质是可学习的模型"。本设计
  "边权 = 静态先验 ⊕ 学习后验"是同一哲学的图版。
- **学习型 agent memory 策略**：AgeMem / "Learn to Memorize"（arxiv 2508.16629）等——agent 自学
  "何时摘要、丢什么"的记忆策略（但学的是**离散管理动作**，不是**图上连续边权**）。

**相同**

- 中心洞见——"**访问轨迹是最值钱的信号，挖它做预取/缓存/工作集估计**"——是内存系统与本设计
  共有的根命题。
- "从 trace 学转移概率来预取"与 Markov/correlation 预取器**机制同构**。
- "静态先验 ⊕ 学习后验"与 learned index"模型 + fallback"、学习型置换"oracle 标签 + 在线分类"
  是同一思路。

**不同（这是本设计最有意思的迁移）**

- **被预取的对象不同**：硬件预取的是 cache line / 内存地址；本设计预取的是
  **typed graph 上的语义节点（符号/文件/诊断）**。把成熟的硬件预取范式搬到"语义图 + LLM
  context"层，这个**跨层迁移本身没找到现成同款**。
- **训练信号不同**：硬件预取器在固定地址空间上学；本设计在**带 typed edge 的语义图**上学，
  边权融合了 scanner 的结构先验——这是硬件预取器没有的"先验骨架"。
- **成本结构不同且更极端**：硬件缺页损失纳秒级，本设计的"缺页"= 一个 LLM round-trip + 重处理
  token（一个 turn）。本设计明确据此论证"预取质量是生死线"——这个**成本不对称**让"学习型预取"
  的收益曲线和硬件场景很不一样（值得更激进地预取/更怕误取）。

**新颖性判定**：**这是本设计最强的原创点之一。** 单看"从 trace 学预取/置换"是成熟领域；但
"**把硬件预取器/learned-index 范式系统性迁移到 LLM 语义 context 图的边权学习**"，且配上
"LLM 缺页贵几个数量级"的成本论证——**没检索到直接对应的现有工作**（最近的 AgeMem 等学的是
离散记忆动作，不是图边权；AttentionPredictor 学的是 KV-cache token，不是语义图）。这是真新点。

**可借鉴（直接可拿）**

- **Markov/correlation 预取器的具体数据结构**（转移表、置信度、prefetch degree/distance）可
  直接做本设计"访问后验"那一半的实现蓝本。
- **Hawkeye 的 "Belady oracle 标签 → 训在线分类器" 训练范式**几乎可照搬：本设计的 ops 流
  历史里，"哪个节点本可命中"是事后可算的 oracle，可据此训边权——这是把 Hawkeye 思路落到图上
  的清晰路径。
- learned index 的核心教训——**"模型 + 兜底（永远保留确定性 fallback）"**——直接对应本设计
  "静态先验始终在场、学习后验只是叠加"，应坚持（避免学崩时无底线）。
- 风险提示（从这些文献继承）：**冷启动 / 分布漂移 / 误预取惩罚**是学习型预取的老大难。本设计
  已自觉"新项目 compile 更差是没攒够 trace 不是 bug"——这是对的，但要防"在错误 trace 上学偏"
  形成自我强化的坏预取（对应缓存里的 pollution）。

---

## 支柱 7 — 抽取优先于截断 / Diagnostic-as-edge

**最接近的 prior art**

- **LSP（Language Server Protocol）`textDocument/publishDiagnostics`**（Microsoft）：把诊断
  标准化成 `{range(file,line), severity, code, message, relatedInformation}` 结构对象。本设计的
  Diagnostic 节点字段与之几乎一一对应。
- **SARIF（Static Analysis Results Interchange Format, OASIS v2.1.0）**
  （<https://docs.oasis-open.org/sarif/sarif/v2.1.0/>）：机器可读 JSON 诊断交换格式，含
  `result` + `location(physicalLocation: file, region: line)` + `relatedLocations` + rule。
  GCC（`-fdiagnostics-format=sarif`）、MSVC（`/experimental:log`）、Clang 等已原生支持。
  **这正是"诊断即结构化、带指回 file:line 的位置链接"的工业标准**。
- **RIG/SPADE 的 coverage 边**（arxiv 2601.10112）：test→component 的结构化 coverage edge，与
  本设计 Cover/Diagnostic 边同精神。
- 结构化日志/可观测性（OpenTelemetry 等）：ingest 时即结构化、而非存 blob 后再 parse。

**相同**

- "**在 ingest 那刻把工具输出解析成结构化诊断，而非存 raw blob 再截断**"——这正是 LSP/SARIF
  的存在理由：诊断从一开始就是带 location 的结构对象。
- "诊断带一条指回 `file:line` 的链接"= SARIF 的 `physicalLocation` / LSP 的 `range`。

**不同（本设计的增量）**

- **诊断不是终点，而是图里的一等边 + 反馈闭环**：本设计的 Diagnostic edge 让"被指向的 File 在
  下一次 compile 扩散里自动成为高相关种子"——**exec 出错 → 错误成边 → 边把相关代码喂进 context
  → agent 修 → 再 exec** 的闭环。LSP/SARIF 只定义诊断的**表示/交换**，不定义"诊断如何反向驱动
  上下文检索"。这个**把诊断接进检索扩散的闭环**是新颖组合。
- **归 bundle extractor 契约**：把"懂 cargo 报错的 parser / 懂 pytest traceback 的 parser"做成与
  scanner 并列的 bundle 契约——是本设计的工程封装（SARIF 是格式，不是这种插件契约）。

**新颖性判定**："结构化诊断而非 blob"**是成熟做法（LSP/SARIF 多年标准）**，不该宣称原创。
增量在"**Diagnostic-as-edge + 自动点亮相关代码的检索闭环**"——这个闭环是有价值的新组合。

**可借鉴（直接可拿）**

- **直接以 SARIF 为 Diagnostic 节点/边的内部数据模型**：复用其 `result/location/relatedLocations`
  schema，省得自定义；且能直接吃 GCC/Clang/MSVC/众多 linter 的原生 SARIF 输出 = 大量工具"免费"
  接入本设计的 extractor。
- 对没有 SARIF 输出的工具（如 cargo/pytest 的人类可读输出），可参考社区已有的
  **"工具输出 → SARIF" 转换器**生态（很多 linter 都有），减少自写 parser。
- 本设计宣称的 100:1 token 压缩，可用 SARIF 既有实践佐证其量级合理。

---

## 支柱 8 — CEP 四边界 ↔ 内存保护（authority/context/audit/lifecycle）

**最接近的 prior art**

- **Capability-based security / object capabilities**（Dennis & Van Horn 1966；后续 EROS、
  seL4、CHERI）：用不可伪造的 capability token 授权对资源的具体操作——对应本设计 `authority`
  边界。**awesome-ocap**（<https://github.com/dckc/awesome-ocap>）是入口。
- **CHERI**（Cambridge/SRI）：在 ISA 层做 capability，硬件强制 fine-grained 内存保护 +
  compartmentalization——是"capability ↔ 内存保护"最权威的现代实现。
- **Information Flow Control (IFC)**（Denning 1976 格模型；后续 Jif、HiStar、Flume）：
  跟踪/约束信息流动，对应本设计"读无关敏感路径""egress"那类 context/authority 关注。
- **MMU / 段式保护 / ECC / 缓存一致性协议（MESI 等）**：本设计逐条映射的内存机制本体。

**相同**

- 四边界 ↔ 内存保护的映射在直觉上自洽：authority↔页权限、context↔段界/地址空间、audit↔ECC/
  校验、lifecycle↔缓存一致性。capability/IFC 文献正是"谁能对什么资源做什么操作 + 信息能流到
  哪"的成熟理论，与 authority/context 边界同源。

**不同**

- 这套映射目前是**类比/助记 + 设计自检工具**，不是形式化执行机制。capability/IFC/CHERI 有
  **形式化的强制模型与（部分）机器验证**（seL4、CHERI 有形式证明）；本设计的 CEP 校验是
  turn 末的工程校验 + 恢复梯子，尚无形式化保证。
- 本设计 audit↔ECC、lifecycle↔缓存一致性这两条映射较"软"，目前更像修辞；authority↔capability、
  context↔IFC 这两条映射**有真实理论可对接**，更承重。

**新颖性判定**：安全机制本体（capability/IFC）**是成熟领域**，不原创。"四边界整齐映射内存保护"
是**有启发性的助记框架**，但目前是类比而非机制——不宜当成已实现的安全保证来宣传。真正要落地的
是把 authority/context 边界**接到真正的 capability/IFC 机制上**。

**可借鉴**

- **authority 边界直接用 object-capability 模型实现**（不可伪造 token 授权工具/路径访问），比
  自造一套权限检查更稳、有成熟理论背书。本设计"Authority 红线：框架可撤销不可批准、绕过边界
  的违规必上人"——这与 capability 的"无 capability 即无权"是一致的安全姿态。
- **context 边界可借 IFC 的格/标签模型**形式化"改动是否在声明范围内 / 读了无关敏感路径"。
- seL4/CHERI 的经验提醒：**形式化的边界才有真保证**；若 CEP 要做成安全卖点，audit/lifecycle 两
  条软映射需要补上可验证的机制，否则只是好看的类比。

---

## 支柱 9 — microVM + CoW 快照做 agent sandbox

**最接近的 prior art（这一层最成熟、选择最多）**

- **Firecracker**（AWS, NSDI 2020）：Lambda/Fargate 的 microVM，~125ms 启动，KVM 硬件隔离，
  **snapshot/restore + MAP_PRIVATE 的 CoW 内存恢复 + on-demand page-in**
  （<https://github.com/firecracker-microvm/firecracker> snapshotting 文档）。
- **gVisor**（Google）：用户态内核拦 syscall，隔离在 syscall 层（Modal 用它）。
- **Kata Containers**：OCI 兼容的 microVM 容器。
- **libkrun / microsandbox**：本设计 v1 选型；libkrun 是嵌入式 microVM runtime。
- **产品层**：E2B、Modal、Daytona、Fly.io Sprites 等"agent code execution sandbox"
  （2026 综述多篇，如 Northflank/Spheron/Blaxel 的对比文）。Firecracker 已是跑不可信 LLM
  生成代码的事实标准。

**相同**

- "microVM 硬件隔离 + CoW 快照做快速 spawn/restore" 是**当前 agent sandbox 的主流共识做法**，
  本设计与之完全一致。reflink/overlay CoW、snapshot 复用、cold boot 兜底都是标准手法。

**不同**

- 本设计 sandbox 的**特殊要求是"数据面无状态 + fanotify 事件流回 host 建图"**（支柱 4），并把
  snapshot 纳入"三链同步（project HEAD / .uninode HEAD / sandbox snapshot）"的一致性协议。
  通用 sandbox 产品不关心"图状态一致性"这层——本设计是把成熟 sandbox 当**数据面底座**，在其上
  叠了自己的控制面语义。
- "snapshot 失败 → 下个 turn cold boot 兜底"是本设计的**降级策略**，与 Firecracker 的 CoW
  restore 是互补的（前者是控制面容错，后者是性能机制）。

**新颖性判定**：sandbox 隔离 + CoW 快照**毫无原创性可言，是高度成熟的红海**——应**直接选用现成
方案**（本设计已正确地选了 microsandbox/libkrun，并实测过本机 IBT 兼容）。本设计的价值不在
sandbox 本身，而在"控制面/数据面分离 + 三链一致性"这层编排。

**可借鉴**

- **Firecracker 的 snapshot/CoW 设计（MAP_PRIVATE + on-demand page-in）** 是性能金标准，本设计
  对"warm 在 project HEAD、静止态 upper==lower、turn 末摊销 Setup"的诉求，可直接对标
  Firecracker 的 snapshot 复用模型。
- E2B/Modal/Daytona 的**公开架构与踩坑**（virtiofs 性能、镜像分发、并发 spawn）可省大量试错；
  本设计 fanotify-on-virtiofs 高并发未压测的风险，这些产品多半已有数据。

---

## 支柱 10 — 工作集 / 颠簸 / Belady lookahead 做上下文管理

**最接近的 prior art**

- **Working set theory**（Peter J. Denning, "The Working Set Model for Program Behavior",
  CACM 1968；50 年综述见 "Working Set Analytics", ACM Computing Surveys 2021）：working set =
  过去时间窗 T 内引用的页集合；据此分配内存可**抵抗颠簸**、近最优吞吐。本设计"本质工作集 >
  窗口 → 颠簸"**直接就是 working set 理论**。
- **Belady's MIN / 异常**（László Bélády, IBM, 1966）：最优置换 = 淘汰"未来最久不用"的页；
  需要未来引用串（离线最优）。本设计"declared plan = 未来引用串 → Belady 式预取/置换"**正是
  Belady MIN 的应用**。
- **CodePlan**（Microsoft Research, "Repository-level Coding using LLMs and Planning",
  ACM SE / arxiv 2309.12499）：**这是 code 域里最贴近的 prior art**——构建 **plan graph**
  （每节点=一个待办的编辑义务，边=因果顺序），用**增量依赖分析 + change may-impact 分析**
  自适应扩展计划图，再逐节点喂 LLM。这与本设计"declared plan 近似未来引用串 + 规划与内存管理
  合一 + change-set/impact 驱动 context"**高度同构**。
- **Navigation Paradox / CodeCompass**（arxiv 2602.20048）+ **ContextBench**：见下方"跨支柱"。
- 学习型置换（Hawkeye/PARROT，支柱 6）也是"逼近 Belady"的一条线。

**相同**

- "工作集 > 窗口 → 颠簸（缺页率高 + 进度低）"= Denning 工作集理论的颠簸判据，逐条对应。
- "declared plan = 未来引用串，做 Belady lookahead"= Belady MIN 用未来串做最优置换的直接套用。
- "分解任务 = 工作集分区（让每块塞进 RAM）"= 工作集理论里"按 locality 分配内存"的应用。
- CodePlan 已经在 code 域实践了"plan graph + impact 分析驱动逐步上下文"——与本设计的
  declared-plan/change-set 思路撞车。

**新颖性判定**：working set / Belady / thrashing **是 60 年的 OS 经典，CodePlan 已把"计划图 +
impact 分析"落到 repo-level coding**——本设计这一层**不是原创，是经典理论 + CodePlan 思路的
应用与重述**。真正的差异点是把它**统一进 virtual-context 内存模型**（"规划与内存管理合一"这句
话本身是好的统一视角，但底层概念都是现成的）。

**可借鉴（直接可拿）**

- **Denning 工作集的颠簸检测判据**（缺页率 vs 进度）可直接做本设计颠簸检测的量化指标。
- **Belady MIN 作为离线 oracle**：和支柱 6 合流——declared plan 给近似未来串，可用它当
  Belady oracle 训练/校准预取与置换（这正是 Hawkeye/PARROT 的路子）。
- **直接研读 CodePlan**：它的"增量依赖分析 + change may-impact 分析 + 自适应扩展 plan graph"
  几乎是本设计 CEP `context` 边界（"改动是否在声明范围内"）+ declared-plan 预取的现成蓝本。
  本设计与其最大差异是 CodePlan 是 symbolic planner 驱动、本设计是 LLM-loop + 框架换页器——
  但 impact 分析那套可直接借。

---

## 跨支柱重点：三个最赌的点的检索结论

### A. 有没有人把 LLM 上下文管理显式当虚拟内存/换页 + 学习型预取（支柱 3+6+10）

- **虚拟内存/换页隐喻本身：有，且已成范式。** MemGPT（2310.08560）立了旗，AIOS（2403.16971）
  把 LLM 当 OS，综述（2507.13334 / 2603.07670）已把"hierarchical virtual context"列为标准
  机制族。**结论：这个隐喻不能宣称原创。**
- **但"框架侧透明换页器 + 结构化子图换入 + 从 agent 自身 ops trace 学习型预取（硬件预取器范式）
  + LLM-缺页成本论证"这套合成，没检索到直接同款。** 现有学习型记忆（AgeMem / Learn to
  Memorize, 2508.16629）学的是**离散管理动作**（何时摘要/丢弃），不是**语义图上的连续边权
  预取**；AttentionPredictor（2502.04077）学的是 **KV-cache token**，是另一层。
- **结论（高置信）：支柱 3+6+10 的"做到什么程度"——隐喻和工作集/Belady 部分是成熟的、必须直接
  学；而"把硬件预取器/learned-index 范式迁移到语义 context 图的边权 + 透明换页"是真空白点，是
  本设计最值得赌、也最该尽快做 spike 验证的原创主张。** 风险：误预取惩罚极高（一个 turn），且
  容易在坏 trace 上自我强化（cache pollution 的语义版），必须有静态先验兜底 + 误预取的负反馈。

### B. 有没有人用 OS 级事件（fanotify/eBPF/ptrace）实时建代码依赖/provenance 图喂 LLM（支柱 4）

- **"OS 事件 → 自动依赖图"：非常成熟。** forward build system（rattle + fsatrace, OOPSLA 2020 /
  2007.12737）、eBPF build provenance（Bomfather 2503.02097）、Bazel hermetic sandbox、安全
  provenance（ProPatrol 等）都是。**底层观测技术必须直接学，别自创。**
- **"实时 FS 事件 → typed graph → 直接喂 LLM context 编译 + 当 CEP audit 边界"：这个下游用途是
  新组合。** AgentSight（eBPF 给 AI agent 做可观测）最沾边，但它做的是**约束/监控**，不是
  **建图喂上下文检索**。
- **结论：技术红海，组合用途有新意。** 该直接复用 build-system 圈"哪些访问算依赖、伪依赖处理、
  virtiofs 可靠性"的全部经验；eBPF 作为 fanotify 撞墙后的升级路径备好。

### C. 图扩散 vs 向量 RAG 在 code context 检索上的实测结论（支柱 2+5）

- **答案是"看任务"，不是某方一边倒。** 关键证据：
  - **Navigation Paradox（CodeCompass, 2602.20048）**：图结构导航在
    **architecture-heavy / hidden-dependency** 任务上胜出；但在**纯语义任务**上 BM25 反而强
    （100% vs 90%），图在隐藏依赖任务上对 vanilla 基本无优势（78.2% vs 76.2%）。
  - **ContextBench**（2026）/ 多篇 SWE 上下文检索评测：**embedding 检索在很多语义定位任务上很强**
    （有报到 embedding 41.7% vs in-context/AST 36.1%）；但**把代码图结构纳入检索能跨语言一致
    提升**。
  - aider 的实践证据：PageRank repo map 比朴素塞文件**编辑准确率显著更高**——支持图法在
    structure-aware 场景的价值。
- **结论（务实）：图法在"架构/跨文件隐藏依赖"场景赢，向量/BM25 在"语义近似定位"场景赢。最佳
  实践是混合**（HippoRAG/SYNAPSE 都用 embedding + 图遍历的三路混合）。本设计"播种用词法/语义命中
  （含可选 embedding），召回靠图扩散"的双段设计**方向正确，但不应宣称"图法全面碾压向量 RAG"**
  ——证据不支持这种绝对化，且本设计在纯语义定位上可能需要保留 embedding/BM25 这一路播种。

---

## 总体新颖性评估

### 真正可能原创 / 差异化的点

1. **从 agent 自身 ops trace 学习语义 context 图的边权（支柱 6 核心）。** 把 Markov/correlation
   预取器、Hawkeye/PARROT 学习型置换、Kraska learned-index 的范式**系统性迁移到 typed code/
   knowledge graph 的边权**，并配"LLM 缺页 = 一个 round-trip、成本不对称"的论证。检索未见直接
   同款。**这是最值得投入的原创主张。**（风险见 A 的 pollution 警告。）
2. **统一的开放 node 模型把 exec/诊断/web/code 收进同一张持久增量图，并直接驱动 context 编译 +
   CEP 边界。** 单看每块都有 prior art（PROV / build graph / SARIF / 代码图），但**端到端合一 +
   bundle 注册表/extractor 契约**这套封装是新颖的工程综合。
3. **Diagnostic-as-edge 的检索闭环（支柱 7）+ Stage 4 "保真度降级背包 + 按 KV-cache 易变性排序"
   （支柱 5）。** 这两个具体机制是本设计自己的工程贡献（诊断表示本身不新，但"诊断成边→自动点亮
   代码→喂回扩散"的闭环、以及"降保真而非丢弃 + 易变性排序保前缀"是有新意的细节）。
4. **控制面/数据面分离 + 三链一致性（project/.uninode/snapshot）+ 四阶恢复梯子。** 编排层的
   设计；sandbox/CoW 本身不新，但这套一致性与容错协议是本设计特有的系统工程。

### 该直接学现成、不要重造的点

- **支柱 2/5 检索骨架**：直接学 **aider 的 tree-sitter + PageRank repo map**（冷启动结构先验）
  和 **HippoRAG/SYNAPSE 的 PPR/spreading-activation + lateral inhibition** 工程；用 spreading
  activation / PPR 的既有术语和实现，别重新发明检索算法。
- **支柱 3 换页器**：读 **MemGPT/Letta** 的实现，尤其 summary 生成/失效、层间一致性的踩坑——
  正对应本设计的 Open Thread。不要宣称发明 virtual context。
- **支柱 4 观测底层**：直接复用 **rattle/fsatrace / Bazel sandbox / eBPF build provenance** 的
  "什么算依赖、伪依赖、virtiofs 可靠性"经验；eBPF 备为升级路径。
- **支柱 7 诊断**：以 **SARIF**（+ LSP diagnostics）为内部数据模型，免费吃掉一大批工具的原生
  结构化输出。
- **支柱 8 安全**：authority 用 **object-capability**、context 用 **IFC 标签**，对接成熟理论而非
  自造权限检查。
- **支柱 9 sandbox**：已正确选用 microsandbox/libkrun；性能对标 **Firecracker snapshot/CoW**。
- **支柱 10 规划/工作集**：直接研读 **CodePlan**（plan graph + may-impact 分析）作为 declared-plan
  与 context 边界的蓝本；颠簸判据用 **Denning 工作集**，离线 oracle 用 **Belady MIN**。

### 最大盲点 / 风险

1. **学习型预取的失败模式（最大技术风险）。** LLM 缺页成本极高 + 容易在坏 trace 上自我强化
   （语义版 cache pollution），且冷启动/分布漂移是学习型预取的历史老大难（Hawkeye/Pythia 文献
   都栽过）。本设计自觉"新项目更差是正常"，但缺**对"学偏后的负反馈/纠偏"机制**的设计。必须先做
   小规模 spike 量化：学习型边权相对纯静态先验到底带来多少 compile 质量提升、误预取率多高。
2. **"图法碾压向量 RAG"是过度声明（证据风险）。** 实测（Navigation Paradox / ContextBench）表明
   是任务相关、且混合最优。文档里"graph 存在的理由就是让 context 编译比朴素 RAG 好"这句要软化——
   在纯语义定位任务上，本设计若彻底放弃 embedding/BM25 播种，可能反而更差。建议保留语义播种这一路。
3. **CEP 四边界目前是类比而非机制（安全风险）。** authority↔capability、context↔IFC 有真理论可接，
   但 audit↔ECC、lifecycle↔缓存一致性偏修辞。若把 CEP 当安全卖点，需要把这些边界落到可验证机制
   （capability/IFC 有形式化先例，本设计目前没有），否则只是好看的助记。
4. **fanotify-on-virtiofs 高并发未验证（工程风险，文档已自列）。** `cargo build` 级别的高并发文件
   事件在 virtiofs 上是否丢包/乱序，直接关系到 audit 边界的完整性（丢包 = 有果无因 = REJECT）。
   build-system 圈对此有经验，应提前借鉴 + 压测；阶 2 的 FS diff 兜底是对的冗余设计。
5. **执行模型/检索算法/虚拟内存隐喻三者单独看都已有强 prior art。** 本设计的价值高度依赖"**把这些
   现成组件合成一个自洽系统 + 学习型预取那一个真新点**"。叙事上应**坦诚承认绝大多数构件是现成的**
   （PROV / spreading activation / MemGPT / Belady / SARIF / Firecracker），把原创性集中主张在
   "学习型语义预取 + 统一图 + 检索闭环"这几处——这样更可信，也更聚焦工程投入。

---

## 引用索引（可二次核验锚点）

- W3C PROV-DM: <https://www.w3.org/TR/prov-dm/> ｜ Primer on Provenance, ACM Queue: <https://queue.acm.org/detail.cfm?id=2602651>
- aider repo map: <https://aider.chat/docs/repomap.html> ｜ tree-sitter+PageRank 说明: <https://aider.chat/2023/10/22/repomap.html> ｜ RepoMapper: <https://github.com/pdavis68/RepoMapper>
- LocAgent: arxiv 2503.09089 ｜ Code Graph Model (CGM): arxiv 2505.16901 ｜ RIG/SPADE: arxiv 2601.10112 ｜ Codebase-Memory: arxiv 2603.27277
- MemGPT (LLMs as OS): arxiv 2310.08560 ｜ AIOS: arxiv 2403.16971 ｜ vLLM PagedAttention (SOSP'23): <https://en.wikipedia.org/wiki/PagedAttention> ｜ Context Engineering 综述: arxiv 2507.13334 ｜ Memory for Autonomous LLM Agents 综述: arxiv 2603.07670
- Build Scripts with Perfect Dependencies (OOPSLA'20): arxiv 2007.12737 ｜ rattle: <https://github.com/ndmitchell/rattle> ｜ fsatrace: <https://github.com/jacereda/fsatrace> ｜ Bomfather (eBPF): arxiv 2503.02097 ｜ AgentSight: <https://medium.com/@yunwei356/agentsight-keeping-your-ai-agents-under-control-with-ebpf-powered-system-observability-6db90abec6e1>
- Spreading activation: Collins & Loftus 1975 (Psychological Review)；Crestani 1997 (AI Review, IR 综述) ｜ HippoRAG (NeurIPS'24): arxiv 2405.14831, <https://github.com/OSU-NLP-Group/HippoRAG> ｜ SYNAPSE: arxiv 2601.02744
- Markov prefetcher: Joseph & Grunwald, ISCA 1997 ｜ Hawkeye: Jain & Lin 2016 ｜ Glider: MICRO 2019 ｜ PARROT (ICML'20): <http://proceedings.mlr.press/v119/liu20f/liu20f.pdf> ｜ Pythia: arxiv 2109.12021 ｜ Learned Index (SIGMOD'18): arxiv 1712.01208 ｜ Learn to Memorize: arxiv 2508.16629
- LSP publishDiagnostics（Microsoft LSP spec）｜ SARIF v2.1.0 (OASIS): <https://docs.oasis-open.org/sarif/sarif/v2.1.0/>
- Capability security / awesome-ocap: <https://github.com/dckc/awesome-ocap> ｜ CHERI（Cambridge/SRI）｜ IFC: Denning 1976 格模型
- Firecracker（NSDI'20）: <https://github.com/firecracker-microvm/firecracker> ｜ gVisor / Kata / E2B / Modal / Daytona（2026 sandbox 综述多篇）
- Working set: Denning, CACM 1968；Working Set Analytics, ACM CSUR 2021 ｜ Belady's MIN: Bélády, IBM 1966 ｜ CodePlan: arxiv 2309.12499 ｜ Navigation Paradox (CodeCompass): arxiv 2602.20048
