# 路线图

## 已完成

- [x] Phase 1: 内核瘦身 — 5 crate workspace, net -5000 行
- [x] Phase 2: 内核收敛 — 15 bug 修复, HTTPS, thinking mode, catalog base_url
- [x] Phase 2: Transport trait 统一、ErrorClassifier 合并、HTTP client 共享
- [x] Phase 3: Dogfooding — 用 LATTICE 开发 LATTICE
- [x] Agent runtime: run() 自动 tool loop, context trimming, sandbox 安全
- [x] 异步 Memory trait + SqliteMemory (FTS5), 17 tools
- [x] lattice-harness: AgentProfile (TOML), Pipeline, AgentRunner
- [x] lattice-plugin: Plugin trait, Behavior trait, PluginRunner
- [x] lattice-cli + lattice-tui

## 进行中

- [ ] Phase 4: 类型化插件 — Input/Output trait, to_prompt/from_output
- [ ] Python handoff, 多 agent 编排

## 计划

- [ ] Phase 5: Nix 范式 — lockfile, 内容寻址缓存, derivation 模型

## 详细

- [ROADMAP.md](../../ROADMAP.md)
- [ideas.md](../../lattice-core/docs/ideas.md)
