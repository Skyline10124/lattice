# 路线图

## 已完成

- [x] Phase 1: 内核瘦身 — 5 crate workspace, net -5000 行
- [x] Phase 2: 内核收敛 — 15 bug 修复, HTTPS, thinking mode, catalog base_url
- [x] Phase 2: Transport trait 统一、ErrorClassifier 合并、HTTP client 共享
- [x] Phase 3: Dogfooding — 用 LATTICE 开发 LATTICE
- [x] Agent runtime: run() 自动 tool loop, context trimming, sandbox 安全
- [x] memory + token-pool crate 合并入 agent/harness
- [x] Agent tools 从 17 精简到 7（read_file, grep, write_file, list_directory, bash, patch, web_search）
- [x] lattice-harness: AgentProfile (TOML), Pipeline, AgentRunner, handoff rule engine, fork 并行
- [x] lattice-plugin: Plugin trait（空架子，待真实插件打磨）
- [x] lattice-cli + lattice-tui
- [x] Gemini SSE streaming 实现
- [x] 蓝军安全审计 — 36 个漏洞发现并归档

## 进行中

- [ ] Phase 4: 类型化插件 — Input/Output trait, to_prompt/from_output
- [ ] Python handoff, 多 agent 编排
- [ ] 安全漏洞修复 — 7 CRITICAL + 9 HIGH（sandbox 重构优先）

## 计划

- [ ] Phase 5: Nix 范式 — lockfile, 内容寻址缓存, derivation 模型
- [ ] **Phase 6: 安全围栏与沙箱架构升级** — 四层围栏 + CubeSandbox 硬件隔离
  - [ ] **围栏层1: 输入校验** — tool call 在沙箱前过结构化解构（command → 程序名+参数列表，path → canonicalize 后检查）
  - [ ] **围栏层2: 执行沙箱** — 当前进程内 sandbox.rs（短期）→ 远期 CubeSandbox（RustVMM/KVM）
    - [ ] 评估 KVM 可用性和 CubeSandbox 单机部署方案
    - [ ] 实现 lattice-sandbox crate 作为 CubeSandbox SDK 的 Rust wrapper
    - [ ] Migration path: 保留 DefaultToolExecutor API，底层切到 CubeSandbox
  - [ ] **围栏层3: 输出过滤** — tool 执行结果回传前脱敏（API key/私钥/本地路径）、大小截断、格式校验
  - [ ] **围栏层4: 策略引擎** — rate limit / token budget / 审计日志 / 异常检测（频繁 bash 调用 → 告警/阻断）
  - [ ] 模块化: 各层通过 trait 独立、按需开关，作为 `lattice-fence` crate 或 `lattice-harness::fence` 模块
  - [ ] 目标: 一次性解决所有 36 个安全漏洞，围栏系统覆盖 sandbox、core、harness、cli 全线
- [ ] Phase 7: 插件系统完善 — cache、rate-limit、obsidian/MATRINX 对接等 3-4 个真实插件，打磨 Plugin trait
- [ ] **Phase 8: 微 agent 通信总线** — agent 间去中心化通信
  - [ ] 注册平台 — agent 按能力注册、心跳、发现
  - [ ] 消息基元 — invoke / fire-and-forget / pub-sub
  - [ ] 总线拓扑 — 事件总线 vs 消息队列，参考 MiroFish IPC、Matrix Room、Actor Model
  - [ ] 目录结构即拓扑 — 注册表的静态视图
- [ ] **Phase 9: 统一网关** — 路由、鉴权、fallback、计价一层解决
- [ ] **Phase 10: 专家架构** — agent 层面的专家路由（代码专家、材料科学专家、实验数据处理专家）

## 愿景

- [ ] "加一个 `lattice-core = \"1.0\"` 就能跑" — 极致轻量全能推理库
- [ ] Rust 生态的 LangChain 级权威 — 定义 LLM 应用开发范式
- [ ] 微 agent 蜂巢操作系统 — 文件夹即 agent，目录结构即拓扑

## 详细

- [ROADMAP.md](../../ROADMAP.md)
- [ideas.md](../../docs/design/ideas.md)
- [安全审计报告](../../docs/issues/README.md)
