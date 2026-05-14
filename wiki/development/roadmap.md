# 路线图

## 已完成

- [x] Phase 1: 内核瘦身 — 5 crate workspace, net -5000 行
- [x] Phase 2: 内核收敛 — Transport trait 统一、HTTPS 强制、thinking mode、credential warning
- [x] Phase 3: Dogfooding + Agent runtime — Agent::run() 自动 tool loop, 7 tools, SqliteMemory, harness + plugin + CLI + TUI
- [x] Phase 4: 类型化插件 + DAG 编排 — Plugin trait (Input/Output), ErasedPlugin, PluginDagRunner, 11 builtin plugins
- [x] Phase 5: 全 async + Rust native — tokio tools, 纯 Rust grep, SYNC_RT/BUS_RT 删除, Pipeline async
- [x] CLI audit 修复 — sessions/stats/config set 真实实现, `--continue-session`/`--session`/`--no-save`/`--provider`/`list agents`, SHA-256 session IDs, `KNOWN_ENV_VARS` 同步
- [x] 四层安全模型 — HookChain + metacharacters + env overrides + command allowlist

## 进行中

- [ ] Phase 6: Nix 范式 — LATTICE.toml + LATTICE.lock, 内容寻址缓存, derivation 模型, overlay, CubeSandbox

## 计划

- [ ] Phase 7: 安全围栏与沙箱架构升级 — 四层围栏 + CubeSandbox 硬件隔离
- [ ] Phase 8: 插件系统完善 — cache、rate-limit、真实插件打磨
- [ ] Phase 9: 微 agent 通信总线完善 — agent 间去中心化通信
- [ ] Phase 10: 统一网关 — 路由、鉴权、fallback、计价
- [ ] Phase 11: 专家架构 — agent 层面的专家路由

## 技术债务优先级

以下为当前 workspace 视角的摘要。旧 wiki 中的 `lattice-bus`、`lattice-python` 项目属于历史快照，除非对应 crate 被重新引入，否则不作为当前 P0。

### P0 — 必须立即修复

| # | 问题 | 位置 |
|---|------|------|
| 1 | 工具 schema 暴露、capability selection 和 executor 授权需要保持单一策略源 | tools + runtime + orchestrator + plugin |
| 2 | URL/私网地址校验在 core/sandbox 多处存在，需要防止策略漂移 | core security + sandbox security |
| 3 | SqliteMemory save_entry() 静默丢弃错误 (.ok()) | agent memory/sqlite.rs |
| 4 | f64::EPSILON 浮点比较 | plugin orchestration.rs |

### P1 — 应尽快修复

| # | 问题 | 位置 |
|---|------|------|
| 5 | Pipeline 迭代/计数和 dry-run 语义需要持续核对 | orchestrator pipeline.rs |
| 6 | AuditLog log_sync() 后台任务错误处理需要核对 | agent audit.rs |
| 7 | builtin plugin parse_output 重复 | plugin builtin/*.rs |
| 8 | PluginDagRunner 直接构造 Agent（边界泄漏） | plugin dag_runner.rs |

### P2 — 可择时修复

其余 44 项 P2 重构建议参见 [重构总览](../refactoring/overview.md)。

## 愿景

- [ ] "加一个 `lattice-core = \"1.0\"` 就能跑" — 极致轻量全能推理库
- [ ] Rust 生态的 LangChain 级权威 — 定义 LLM 应用开发范式
- [ ] 微 agent 蜂巢操作系统 — 文件夹即 agent，目录结构即拓扑

## 详细

- [ROADMAP.md](https://github.com/Skyline10124/lattice/blob/main/ROADMAP.md)
- [ideas.md](https://github.com/Skyline10124/lattice/blob/main/docs/design/ideas.md)
- [安全审计报告](https://github.com/Skyline10124/lattice/blob/main/docs/issues/README.md)
