# Crate 边界分析

本页描述当前 workspace。旧文档中的 `lattice-bus` 现对应 `lattice-orchestrator`；`lattice-python` 当前不在 workspace 中。

## 依赖方向

```text
lattice-core
  ├─ lattice-prompt
  ├─ lattice-hooks
  ├─ lattice-sandbox
  └─ lattice-tools
       └─ lattice-agent
            ├─ lattice-plugin
            └─ lattice-orchestrator
                 └─ lattice-runtime
                      └─ lattice-cli

lattice-session is consumed by lattice-runtime and lattice-cli.
lattice-plugins depends on lattice-plugin.
```

## 已收敛的历史问题

- `BehaviorMode` / `YoloSandboxPolicy` 已位于 `lattice-core::types`，不再由 agent 内部模块向上层泄漏。
- handoff 规则已由 `lattice_core::handoff` 提供，`lattice-plugin::orchestration` 和 `lattice-orchestrator::handoff_rule` 只做复用/再导出。
- 旧 `lattice-bus` 名称已由 `lattice-orchestrator` 替代。

## 当前边界问题清单

### B1 — `PluginAgent` trait 的归属

**位置**: `lattice-agent/src/lib.rs`, `lattice-plugin/src/lib.rs`

`PluginAgent` 仍定义在 `lattice-agent`，但它表达的是 plugin runner 对 agent-like 对象的最低需求。`lattice-plugin` 通过 re-export 使用它，导致 plugin API 依赖 agent crate 的具体接口位置。

**建议**: 将 `PluginAgent` trait 移到 `lattice-plugin` 或更小的 shared interface module，`lattice-agent` 为 `Agent` 提供 impl。

### B2 — `PluginDagRunner` 直接构造 Agent

**位置**: `lattice-plugin/src/dag_runner.rs`

Plugin DAG runner 自己解析模型、构造 `Agent`、合并工具、创建 `DefaultToolExecutor`。这让 plugin crate 需要理解 agent construction、tool capability policy 和 executor wiring。

**建议**: 让 `PluginDagRunner` 接受 `AgentFactory`/callback，由 `lattice-orchestrator` 或 `lattice-runtime` 提供 agent construction。

### B3 — 工具能力策略分散

**位置**: `lattice-tools/src/capabilities.rs`, `lattice-orchestrator/src/assembly.rs`, `lattice-plugin/src/dag_runner.rs`, `lattice-runtime/src/lib.rs`, `lattice-cli/src/frontend/tui/`

`lattice-tools` 已经有 `ToolPreset`、`ToolBundle` 和 `CapabilitySelection`，但不同 entrypoint 仍需要各自把 tool definitions、registry handlers、sandbox 和 executor policy 组装起来。

**建议**: 继续把“tool capability → schema exposure → executor authorization → sandbox”做成一个深 Module，让 entrypoint 只传 capability selection。

### B4 — sandbox/security helper 重复

**位置**: `lattice-core/src/security.rs`, `lattice-sandbox/src/security.rs`

`lattice-sandbox` 为保持 leaf crate 属性复制了一部分 private/reserved IP 判断逻辑。短期可接受，但安全策略变动容易出现漂移。

**建议**: 明确一个 canonical security policy source，或增加跨 crate characterization tests，确保 core base URL policy 和 sandbox URL policy 行为一致。

### B5 — Orchestrator `lib.rs` 仍然偏宽

**位置**: `lattice-orchestrator/src/lib.rs`

`lib.rs` 同时公开 orchestrator trait、request/response/event 类型、config、delivery policy、handler helper 和 in-memory 实现。虽然已有 `pipeline.rs`、`runner.rs`、`profile.rs` 等模块拆分，但 `lib.rs` 仍是较宽的 Implementation surface。

**建议**: 将 in-memory implementation、types、handler helpers 继续下沉到独立 modules，`lib.rs` 只保留 re-export 和少量核心 trait。

### B6 — Runtime 与 Orchestrator 都能构造 Agent

**位置**: `lattice-runtime/src/lib.rs`, `lattice-orchestrator/src/assembly.rs`

`AgentFactory` 是 profile-backed canonical path；`Runtime::build_agent()` 是 CLI/TUI convenience path。两个路径都需要处理 security、tools、capabilities 和 prompt providers，容易产生策略差异。

**建议**: 让 runtime direct-agent path 复用同一个 lower-level construction Module，或把差异显式编码为两个小 Adapter。

## 边界清晰度评分

| Crate | 清晰度 | 主要观察 |
|-------|--------|----------|
| `lattice-core` | ★★★★☆ | shared types 和 handoff 已下沉；security policy 仍需和 sandbox 保持一致 |
| `lattice-prompt` | ★★★★☆ | prompt compiler 和 provider interface 边界清楚 |
| `lattice-sandbox` | ★★★★☆ | 独立策略 crate 清楚；与 core security 有重复 |
| `lattice-tools` | ★★★☆☆ | capability Module 是正确方向；executor/registry/schema policy 仍需更集中 |
| `lattice-agent` | ★★★☆☆ | Agent loop 清楚；`PluginAgent` trait 归属仍不理想 |
| `lattice-plugin` | ★★★☆☆ | plugin API 清楚；DAG runner 直接构造 Agent 边界偏宽 |
| `lattice-orchestrator` | ★★★☆☆ | profile/pipeline 方向清楚；`lib.rs` 和 construction policy 偏宽 |
| `lattice-runtime` | ★★★★☆ | 作为 application seam 合理；需避免 direct path 和 profile path 策略漂移 |
| `lattice-session` | ★★★★☆ | session graph 和 store interface 独立 |
| `lattice-cli` | ★★★☆☆ | 用户入口清楚；TUI 是大模块，后续可继续拆分 |

## 建议的重构方向

1. 把 tool capability policy 做成更深的 Module。
2. 将 `PluginAgent` 接口移动到 plugin-owned interface。
3. 让 plugin DAG agent construction 依赖 orchestrator/runtime 提供的 factory。
4. 为 core/sandbox security policy 增加一致性测试。
5. 继续压缩 `lattice-orchestrator/src/lib.rs` 和 TUI 大模块。
