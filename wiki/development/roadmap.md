# 路线图

## 已完成

- [x] Phase 1: 内核瘦身 — 5 crate workspace, net -5000 行
- [x] Phase 2: 内核收敛 — Transport trait 统一、HTTPS 强制、thinking mode、credential warning
- [x] Phase 3: Dogfooding + Agent runtime — Agent::run() 自动 tool loop, 7 tools, SqliteMemory, harness + plugin + CLI + TUI
- [x] Phase 4: 类型化插件 + DAG 编排 — Plugin trait (Input/Output), ErasedPlugin, PluginDagRunner, 11 builtin plugins
- [x] Phase 5: 全 async + Rust native — tokio tools, 纯 Rust grep, SYNC_RT/BUS_RT 删除, Pipeline async
- [x] CLI audit 修复 — sessions/stats/config set 真实实现, `--continue-session`/`--no-save`/`--provider`/`list agents`, SHA-256 session IDs, `KNOWN_ENV_VARS` 同步

## 进行中

- [ ] Phase 6: Nix 范式 — LATTICE.toml + LATTICE.lock, 内容寻址缓存, derivation 模型, overlay, CubeSandbox

## 计划

- [ ] Phase 7: 安全围栏与沙箱架构升级 — 四层围栏 + CubeSandbox 硬件隔离
- [ ] Phase 8: 插件系统完善 — cache、rate-limit、真实插件打磨
- [ ] Phase 9: 微 agent 通信总线完善 — agent 间去中心化通信
- [ ] Phase 10: 统一网关 — 路由、鉴权、fallback、计价
- [ ] Phase 11: 专家架构 — agent 层面的专家路由

## 愿景

- [ ] "加一个 `lattice-core = \"1.0\"` 就能跑" — 极致轻量全能推理库
- [ ] Rust 生态的 LangChain 级权威 — 定义 LLM 应用开发范式
- [ ] 微 agent 蜂巢操作系统 — 文件夹即 agent，目录结构即拓扑

## 详细

- [ROADMAP.md](../../ROADMAP.md)
- [ideas.md](../../docs/design/ideas.md)
- [安全审计报告](../../docs/issues/README.md)
