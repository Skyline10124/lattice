# 边界问题清单

本清单只列当前 workspace 中仍有意义的问题。历史 `lattice-bus` / `lattice-python` 项目已移到 [Historical API Notes](../api/historical.md)。

## B1 — `PluginAgent` trait 的归属

**代码位置**

- 定义: `crates/lattice-agent/src/lib.rs`
- 使用/再导出: `crates/lattice-plugin/src/lib.rs`

**影响**

Plugin runner 使用者需要接受 agent crate 的接口位置，即使它只需要一个“能发送带工具消息的 agent-like 对象”。这会让 plugin API 和 agent 内部组织产生不必要耦合。

**修复方向**

将 `PluginAgent` trait 移入 `lattice-plugin`，或放入更小的 shared interface module。`lattice-agent::Agent` 为该 trait 提供 impl。

## B2 — `PluginDagRunner` 直接构造 Agent

**代码位置**

- `crates/lattice-plugin/src/dag_runner.rs`

**影响**

Plugin DAG runner 同时负责模型解析、agent construction、tool merge、executor creation 和 memory wiring。该 Implementation 深度过宽，导致 tool capability 或 sandbox 策略变化需要同步多个 entrypoint。

**修复方向**

让 DAG runner 接受 `AgentFactory` trait 或 `Fn(slot, model, tools) -> Agent` callback。`lattice-orchestrator` / `lattice-runtime` 负责 construction。

## B3 — 工具能力策略分散

**代码位置**

- `crates/lattice-tools/src/capabilities.rs`
- `crates/lattice-orchestrator/src/assembly.rs`
- `crates/lattice-plugin/src/dag_runner.rs`
- `crates/lattice-runtime/src/lib.rs`
- `crates/lattice-cli/src/frontend/tui/`

**影响**

`ToolPreset` / `ToolBundle` 已经提供了能力语言，但 schema exposure、registry handler、executor authorization 和 sandbox wiring 仍跨多个调用路径。任何一个路径漏接策略都会造成行为不一致。

**修复方向**

创建一个 tool execution policy Module，输入 capability selection、registry、sandbox，输出成对的 tool definitions 和 executor。

## B4 — core/sandbox security helper 重复

**代码位置**

- `crates/lattice-core/src/security.rs`
- `crates/lattice-sandbox/src/security.rs`

**影响**

模型 base URL validation 和工具 URL validation 都需要判断私网/保留地址。重复实现会带来策略漂移风险。

**修复方向**

保持重复实现时，增加 shared characterization tests。若依赖方向允许，抽出更底层的 security helper。

## B5 — `lattice-orchestrator/src/lib.rs` 偏宽

**代码位置**

- `crates/lattice-orchestrator/src/lib.rs`

**影响**

`lib.rs` 暴露 trait、types、config、delivery policy、handler helper 和 in-memory implementation。读者进入 orchestrator crate 时需要同时解析 Interface 和 Implementation。

**修复方向**

拆出 `types.rs`、`in_memory.rs`、`handler.rs` 等模块，`lib.rs` 保留 re-export。

## B6 — Runtime direct-agent path 与 profile-backed path 并存

**代码位置**

- `crates/lattice-runtime/src/lib.rs`
- `crates/lattice-orchestrator/src/assembly.rs`

**影响**

`Runtime::build_agent()` 和 `AgentFactory::assemble()` 都会构造 agent。二者面向不同 use case，但工具、sandbox、prompt provider 策略必须保持一致。

**修复方向**

抽出共享 lower-level construction Module，runtime/profile 两边只提供不同 Adapter。
