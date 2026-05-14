# Phase 1: Profile Contract Extraction - Research

**Researched:** 2026-05-14  
**Domain:** Rust workspace profile/schema extraction, strict TOML loading, pure runtime spec lowering  
**Confidence:** HIGH

<user_constraints>
## User Constraints (from CONTEXT.md)

### Locked Decisions

## Implementation Decisions

### Profile File Contract
- **D-01:** A profile is a stable user-facing configuration file for a single runnable agent, not a full runtime spec and not a runtime assembly script.
- **D-02:** Keep the existing and clearer physical semantics: each agent is an agent package folder at `.lattice/agents/<name>/agent.toml` or `~/.lattice/agents/<name>/agent.toml`. Do not introduce `.lattice/profiles/` or `profile.toml` in v1.
- **D-03:** The `agent.toml` file defines one generic agent profile. Neighbor files such as `prompt.md` are allowed inside the same package folder.
- **D-04:** Relative paths inside `agent.toml`, such as `prompt.system_path`, resolve relative to the directory containing that `agent.toml`, not relative to the process cwd.
- **D-05:** Explicit file paths take highest priority. Named lookup checks the project-local agent before the user-global agent. Project-local agents override global agents with the same name.
- **D-06:** When no agent is specified, LATTICE uses the `default` agent. It first looks for `.lattice/agents/default/agent.toml`, then `~/.lattice/agents/default/agent.toml`.
- **D-07:** If no default agent exists, an interactive terminal prompts before creating `.lattice/agents/default/agent.toml`. Non-interactive environments fail with a clear error instead of writing files unexpectedly.
- **D-08:** The generated default agent template must include prompt text and comments telling the user it is an auto-generated template and should be edited for the project, including role, prompt, tools, and security settings.

### Minimal v1 Schema
- **D-09:** The v1 schema is strict allowlist only. Unknown sections or fields are errors. This project has not shipped, so compatibility with old profile fields is not required.
- **D-10:** The allowed top-level sections are `schema_version`, `[agent]`, `[prompt]`, `[tools]`, `[security]`, `[context]`, and `[behavior]`.
- **D-11:** `[agent]` carries stable identity fields such as `name`, `model`, and optional `description`. It must not carry `preset`, `type = "code_agent"`, `skippable`, or `tags`.
- **D-12:** `[prompt]` supports either inline `system` or `system_path`, but not both. `system_path` must exist, must be a file, and must not escape the agent package directory.
- **D-13:** `[security]` only carries the current coarse-grained runtime security intent: `sandbox_mode = "project" | "strict" | "permissive"`. Do not add profile-level approval mode, allowlists, Landlock, audit, or raw approval grants in Phase 1.
- **D-14:** `[context]` may carry simple context toggles such as `project_instructions = true` and `workspace_context = true`. It must not carry `cwd` or `project_root`; cwd/project root come from the runtime invocation context.
- **D-15:** `[behavior]` may carry simple generic turn controls such as `max_turns` and `reasoning_effort`. It must not carry old orchestrator behavior fields such as `behavior_type`, `confidence_threshold`, `max_retries`, or `enforce_sandbox`.

### Crate Boundary
- **D-16:** Create a new narrow `lattice-profile` crate instead of piling schema, loading, discovery, validation, defaults, and lowering into `lattice-runtime`, `lattice-agent`, or the old `lattice-orchestrator` crate.
- **D-17:** `lattice-profile` owns strict schema types, file/package loading, discovery, default template generation, clear profile errors, `system_path` checks, and pure data lowering into the generic turn/build spec.
- **D-18:** `lattice-profile` must not depend on `lattice-runtime`, `lattice-orchestrator`, `lattice-agent`, `lattice-session`, `lattice-plugin`, or CLI/TUI state. It must not construct a live `Agent`, read session stores, execute tools, or load plugins.
- **D-19:** Runtime remains responsible for environment-dependent semantic validation: model alias/provider/credentials, actual tool bundle and tool name existence, sandbox construction, and agent handle construction.
- **D-20:** `lattice-agent` remains a generic agent loop and does not depend on `lattice-profile` or know about profile files.

### Compatibility and Errors
- **D-21:** There is no compatibility layer, warning-ignore mode, dry-run migrator, or automatic migration command for old profiles. Old fields fail fast.
- **D-22:** Explicitly forbidden profile concepts include `[capabilities]`, `[handoff]`, `[orchestrator]`, `[plugins]`, `[pipeline]`, `[subagents]`, `[micro_agents]`, `[memory]`, `agent.preset`, `agent.type = "code_agent"`, `agent.skippable`, `agent.tags`, and old behavior fields.
- **D-23:** Profile errors must be concrete and user-actionable: include the file path, section/field/value that failed, why it is rejected, and the new schema shape to use instead.
- **D-24:** Tests should cover strict rejection of unknown sections/fields and every explicitly forbidden legacy section/field.

### CLI and Coding Experience
- **D-25:** Delete the `lattice code` subcommand rather than preserving it as an alias. Coding is not a special agent type.
- **D-26:** A coding-capable agent is just a normal agent profile with a coding-oriented prompt and selected tool bundles/tools. Users run it through the generic runtime path, for example `lattice run --agent code "fix the failing tests"`.
- **D-27:** Remove or rewrite `coding_agent`, `AgentPreset::Coding`, `code_agent`, coding-preset runtime branches, and tests/evals that only exercise the specialized coding-agent path.

### Tool Capability Vocabulary
- **D-28:** Agent profile v1 exposes tools only through `[tools]`; do not keep `[capabilities]` in the profile schema.
- **D-29:** `[tools]` keeps both `bundles` and `enabled`. Final exposed tools are the union of tools expanded from bundles plus explicitly enabled tool names, deduplicated by runtime/tooling.
- **D-30:** Delete the presets concept aggressively now. Remove `ToolPreset`, `CapabilitySelection.presets`, `coding`, `code-agent`, `reader`, and `minimal` preset paths rather than carrying them forward into the generic profile contract.
- **D-31:** Bundle vocabulary can keep the current `lattice-tools` scope for this phase: `core`, `workspace`, `filesystem`, `editing`, `shell`, `planning`, and `web`.
- **D-32:** `lattice-profile` only validates that `bundles` and `enabled` are arrays of strings. Runtime/`lattice-tools` validates actual bundle/tool existence and produces the semantic errors.

### the agent's Discretion
- The planner may choose exact Rust module names inside `lattice-profile` as long as the crate remains narrow and does not pull in runtime/agent/orchestrator dependencies.
- The planner may choose the exact pure-data spec type name (`AgentTurnSpec`, `AgentBuildSpec`, or similar) as long as it does not encode plugin, handoff, orchestrator, subagent, or coding-preset semantics.

### Deferred Ideas (OUT OF SCOPE)

## Deferred Ideas

None — discussion stayed within phase scope.
</user_constraints>

<phase_requirements>
## Phase Requirements

| ID | Description | Research Support |
|----|-------------|------------------|
| PROF-01 | Developer can define a generic agent profile that describes model, prompt, tool capabilities, behavior, working directory, and security intent without plugin, handoff, orchestrator, or subagent fields. | 使用新的严格 `lattice-profile` TOML schema，只允许 `schema_version`、`[agent]`、`[prompt]`、`[tools]`、`[security]`、`[context]`、`[behavior]`；运行目录由 runtime 调用上下文提供，不写入 profile。 [VERIFIED: .planning/REQUIREMENTS.md] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| PROF-02 | Runtime can lower a profile or CLI default into a pure generic turn/build spec without constructing a live `Agent` or reading CLI/TUI state. | 将 `lattice-orchestrator/src/agent_spec.rs` 里的纯数据思路迁到 `lattice-profile`，去掉 handoff/memory/preset 字段；模型、工具、sandbox 的语义校验留给 runtime/tools。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| PROF-03 | Coding-capable behavior is represented by profile prompt and tool capabilities, not by `AgentPreset::Coding`, `coding_agent.rs`, or a dedicated agent subtype. | 用 `[tools].bundles` 和 `[tools].enabled` 表达 coding 所需工具；当前 `ToolPreset::Coding`、`AgentPreset::Coding`、`runtime::preset`、`crates/lattice-cli/src/coding_agent.rs` 是删除或改写点。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: crates/lattice-runtime/src/preset.rs] [VERIFIED: crates/lattice-cli/src/coding_agent.rs] |
</phase_requirements>

## Project Constraints (from AGENTS.md)

- 这是 Rust workspace；标准门禁是 `cargo build`、`cargo test`、`cargo clippy -- -D warnings`、`cargo fmt --all --check`、定向 `cargo test <test_name>` 和 `cargo bench`。 [VERIFIED: AGENTS.md]
- 文档约束的依赖流是 `hooks -> core -> session/prompt/sandbox/tools -> agent -> plugin -> orchestrator -> runtime -> cli`；新增 `lattice-profile` 必须位于 runtime 之下，不能让低层 crate 依赖上层 crate。 [VERIFIED: AGENTS.md] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- AGENTS/CLAUDE 仍描述 plugin/orchestrator 层；本轮目标明确只写 research，不能顺手更新这些项目文档。 [VERIFIED: AGENTS.md] [VERIFIED: CLAUDE.md]
- 根指令引用 `RTK.md`，但仓库根目录没有该文件。 [VERIFIED: shell `test -f RTK.md`]

## Summary

Phase 1 需要把“用户可编辑的单 agent profile 合约”从旧 orchestrator 路径里抽出来，落到新的窄 crate `crates/lattice-profile`。这个 crate 应拥有严格 TOML schema、文件/包加载、显式路径/名称/default discovery、默认模板文本、具体可操作的错误、`prompt.system_path` 包内路径校验，以及 profile 到纯数据 spec 的 lowering。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

当前最高风险点是旧 `AgentProfile` 和 `AgentTurnSpec` 已经混入太多 v1 不要的语义：`[capabilities]`、`[handoff]`、`[orchestrator]`、`[memory]`、`[plugins]`、`AgentPreset::Coding`、`CapabilitySelection.presets`。这些不是迁移输入，而是新 parser 的拒绝夹具。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

推荐规划顺序是先建独立 `lattice-profile` 和完整 parser/loader/lowering 测试，再按编译需要桥接旧 runtime/orchestrator 使用点，最后处理 `lattice-tools` preset 删除和测试改写。这样可以让 Phase 1 交付稳定 contract，同时避免提前吞下 Phase 2 runtime assembly 和 Phase 3 CLI/TUI UX 的全部爆炸半径。 [VERIFIED: .planning/ROADMAP.md] [VERIFIED: crates/lattice-runtime/src/lib.rs] [ASSUMED]

**Primary recommendation:** 使用 `#[serde(deny_unknown_fields)]` 做结构性 strict schema，并在反序列化前用 `toml::Value` 做 forbidden legacy 字段扫描以生成更好的用户错误；lowering 保持纯数据转换，模型、工具、sandbox 语义校验留给 runtime/tools。 [CITED: https://serde.rs/container-attrs.html] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

## Architectural Responsibility Map

| Capability | Primary Tier | Secondary Tier | Rationale |
|------------|--------------|----------------|-----------|
| Agent profile 文件合约 | `lattice-profile` crate | CLI 负责交互确认 | Phase 决策明确 schema、loading、discovery、default template、errors、path checks、lowering 属于 `lattice-profile`；CLI 只应在创建 default 文件前询问。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| 显式路径/名称/default discovery | `lattice-profile` crate | CLI/runtime 传入 project root 与 global dir | 现有行为在 `AgentRegistry::load_dir` 和 `LatticeDir::discover_with_global`；应保留 package folder lookup，但不要保留 orchestrator config。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] |
| Strict schema 校验 | `lattice-profile` crate | `toml`/`serde` parser | Serde 官方支持 `deny_unknown_fields` 拒绝未知字段；当前 workspace 已用 serde/toml 做配置解析。 [CITED: https://serde.rs/container-attrs.html] [VERIFIED: Cargo.toml] |
| `prompt.system_path` 解析 | `lattice-profile` crate | Filesystem | 旧代码部分基于 profile dir 解析 prompt，但安全边界检查混入 cwd；新行为必须以 agent package dir 为边界。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| Tool bundle/tool-name 语义校验 | `lattice-tools` 和 runtime | `lattice-profile` 只做 string array 形状校验 | D-32 明确 `lattice-profile` 只验证数组字符串，实际 bundle/tool 是否存在由 runtime/`lattice-tools` 负责。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| Sandbox 构建 | `lattice-runtime` | `lattice-sandbox` | Profile 只携带 `sandbox_mode`；当前 runtime 已通过 `build_runtime_security` 映射 `project`、`strict`、`permissive`。 [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| 模型/provider/credential 解析 | `lattice-runtime` / `lattice-core` | CLI credentials config | Profile 只携带 model 字符串；runtime 当前拥有 `resolve_with_provider` 和 credentials-backed `ModelRouter`。 [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| Live `Agent` 构建和 handle 复用 | `lattice-runtime` | `lattice-agent` | runtime handle registry 拥有 live `Agent`，并以 session/workspace/profile/model/security key 复用；profile lowering 不能构造 live agent。 [VERIFIED: crates/lattice-runtime/src/handle.rs] [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| CLI/TUI 命令 UX | `lattice-cli` | `lattice-profile` 提供加载/模板 | 当前 CLI 拥有 `run`、`code`、`new agent`、validate/listing、TUI routing；Phase 1 只需为后续 CLI/TUI cutover 提供通用 profile 输入。 [VERIFIED: crates/lattice-cli/src/main.rs] |

## Standard Stack

### Core

| Library / Crate | Version | Purpose | Why Standard |
|-----------------|---------|---------|--------------|
| `lattice-profile` | `0.1.0` workspace version | 新 strict profile schema、loader、discovery、default template、error、pure lowering。 | Phase 1 context 锁定新窄 crate 边界。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] [VERIFIED: Cargo.toml] |
| `serde` | `1.0.228` locked | 为 strict TOML-facing structs 派生 `Deserialize`/`Serialize`。 | workspace 标准序列化层；Serde 官方支持 `deny_unknown_fields` 拒绝未知字段。 [VERIFIED: Cargo.lock] [CITED: https://serde.rs/container-attrs.html] |
| `toml` | `0.8.23` locked | 解析 `agent.toml` 到 schema structs 或 `toml::Value`。 | 现有 profile/config 路径已使用 TOML，workspace pin `toml = "0.8"`。 [VERIFIED: Cargo.toml] [VERIFIED: Cargo.lock] |
| `thiserror` | `2.0.18` locked | 为 profile loading/validation/lowering 暴露 typed library errors。 | 项目约定：library 用 typed error，CLI glue 用 `anyhow`。 [VERIFIED: Cargo.lock] [VERIFIED: .planning/codebase/CONVENTIONS.md] |
| `lattice-tools` | `0.1.0` workspace version | profile lowering 后负责 bundle/tool name 语义校验和 expansion。 | 当前 bundle vocabulary 和工具暴露策略已在 `capabilities.rs`。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] |
| `lattice-runtime` | `0.1.0` workspace version | 从纯 profile spec 做环境相关的 model/tool/sandbox/handle assembly。 | D-19 明确 runtime 负责模型别名、凭证、实际工具存在性、sandbox 构建和 agent handle。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |

### Supporting

| Library / Crate | Version | Purpose | When to Use |
|-----------------|---------|---------|-------------|
| `tempfile` | `3.27.0` locked | agent package、prompt 文件、default generation、路径逃逸检查的隔离文件系统测试。 | `lattice-profile` 单元测试和后续 CLI 集成测试。 [VERIFIED: Cargo.lock] [VERIFIED: .planning/codebase/TESTING.md] |
| `dirs` | `5.0.1` locked | 如果 `lattice-profile` 直接拥有 `~/.lattice/agents` 展开，可用来查 home/config dir。 | 优先让 caller 传 global path；只有必须内置 home discovery 时才加。 [VERIFIED: Cargo.lock] [ASSUMED] |
| `tracing` | `0.1.44` locked | bulk discovery failure 的非关键诊断。 | 可少量使用；用户可操作错误仍应走 typed `ProfileError`。 [VERIFIED: Cargo.lock] [VERIFIED: crates/lattice-orchestrator/src/registry.rs] |

### Alternatives Considered

| Instead of | Could Use | Tradeoff |
|------------|-----------|----------|
| 新 `lattice-profile` crate | `lattice-runtime::profile` module | 被 D-16 否决；runtime 会变成 schema/loading/defaulting 垃圾桶。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| 新 `lattice-profile` crate | 留在 `lattice-orchestrator` | 被 roadmap 否决；Phase 4 要移除 orchestrator，Phase 1 正是先抽出有用 contract。 [VERIFIED: .planning/ROADMAP.md] |
| Strict allowlist schema | 旧字段兼容 parser | 被 D-09/D-21 否决；项目未发布，不需要兼容旧 profile。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `[tools]` bundles/enabled | `ToolPreset::{Minimal, Reader, Coding}` | 被 D-30 否决；preset 会继续编码 profile-level 行为和 coding-agent 语义。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] [VERIFIED: crates/lattice-tools/src/capabilities.rs] |

**Installation / workspace changes:**

```bash
# 添加 crate 目录和 workspace member；无需引入当前 stack 之外的新第三方依赖。
cargo test -p lattice-profile
```

**Version verification:** 内部 crate 版本通过 `cargo metadata --format-version 1 --no-deps` 核验；外部 locked 版本通过 `Cargo.lock` 核验。 [VERIFIED: cargo metadata] [VERIFIED: Cargo.lock]

## Architecture Patterns

### System Architecture Diagram

```text
Explicit path / named agent / no agent
        |
        v
ProfileSource resolution
  |-- explicit path ----------------------------> agent.toml path
  |-- name -------------------------------------> .lattice/agents/<name>/agent.toml
  |                                                then ~/.lattice/agents/<name>/agent.toml
  |-- none/default -----------------------------> .lattice/agents/default/agent.toml
                                                   then ~/.lattice/agents/default/agent.toml
                                                   else MissingDefault { template, create_path }
        |
        v
Read TOML without side effects
        |
        v
Forbidden-field pre-scan + Serde strict deserialize
        |
        v
Package validation
  |-- prompt.system XOR prompt.system_path
  |-- system_path canonical file inside package
  |-- security.sandbox_mode in project/strict/permissive
  |-- tools.bundles/enabled are string arrays only
        |
        v
LoadedAgentProfile
        |
        v
Pure lowering: LoadedAgentProfile -> GenericAgentSpec
        |
        v
Later runtime phase: model resolution, tool expansion, sandbox construction, Agent handle
```

该流向覆盖 locked 的 project/global 查找顺序、default-agent 行为、strict schema 和 runtime-owned 语义校验。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

### Recommended Project Structure

```text
crates/lattice-profile/
├── Cargo.toml                 # workspace package metadata and narrow deps
└── src/
    ├── lib.rs                 # public exports
    ├── schema.rs              # AgentProfile and section structs
    ├── error.rs               # ProfileError and ProfileLoweringError
    ├── loader.rs              # file/package read + strict parse
    ├── discovery.rs           # explicit/name/default lookup
    ├── template.rs            # generated default agent.toml text
    ├── lowering.rs            # pure profile -> generic spec conversion
    └── tests.rs               # focused unit tests or inline test modules
```

具体模块名是 planner 的自由度；crate 边界和 forbidden dependency list 不是自由度。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

### Pattern 1: Strict Schema With Better Forbidden-Field Errors

**What:** 所有 schema struct 使用 `#[serde(deny_unknown_fields)]`；同时先用 `toml::Value` 扫描显式 forbidden legacy fields，以便给出 D-23 要求的具体错误。 [CITED: https://serde.rs/container-attrs.html] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

**When to use:** 用于 `AgentProfile`、`AgentSection`、`PromptSection`、`ToolsSection`、`SecuritySection`、`ContextSection`、`BehaviorSection`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

```rust
// Source: Serde docs for deny_unknown_fields; field list from Phase 1 context.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub schema_version: u32,
    pub agent: AgentSection,
    pub prompt: PromptSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub security: SecuritySection,
    #[serde(default)]
    pub context: ContextSection,
    #[serde(default)]
    pub behavior: BehaviorSection,
}
```

### Pattern 2: Separate Parsed Profile From Loaded Profile

**What:** `AgentProfile` 保持为 declarative parsed data；`LoadedAgentProfile` 表示已经校验 package path 并解析出 system prompt 内容的加载结果。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

**When to use:** 这样 lowering 不需要读文件，runtime 也能消费已经加载好的 prompt 文本。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

```rust
// Source: Current profile loader reads profile-relative prompt paths; new check must scope to package_dir.
pub struct LoadedAgentProfile {
    pub path: PathBuf,
    pub package_dir: PathBuf,
    pub profile: AgentProfile,
    pub system_prompt: String,
}

pub fn load_agent_toml(path: &Path) -> Result<LoadedAgentProfile, ProfileError> {
    let package_dir = path.parent().ok_or_else(|| ProfileError::MissingPackageDir {
        path: path.to_owned(),
    })?;
    let profile: AgentProfile = parse_strict(path)?;
    let system_prompt = resolve_prompt(package_dir, &profile.prompt)?;
    Ok(LoadedAgentProfile {
        path: path.to_owned(),
        package_dir: package_dir.to_owned(),
        profile,
        system_prompt,
    })
}
```

### Pattern 3: Pure Lowering

**What:** 将已加载 profile 转成普通 spec struct，不能包含 runtime、session、tool execution、plugin、handoff、coding-preset 字段。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

**When to use:** 用在 profile loading 与后续 runtime assembly 的边界。 [VERIFIED: .planning/ROADMAP.md]

```rust
// Source: Current lower_profile_to_turn_spec is pure but still carries legacy fields.
pub fn lower_profile_to_spec(profile: &LoadedAgentProfile) -> Result<AgentBuildSpec, ProfileError> {
    if profile.profile.agent.name.trim().is_empty() {
        return Err(ProfileError::InvalidField {
            path: profile.path.clone(),
            field: "agent.name",
            value: String::new(),
            reason: "must not be empty",
            expected: "non-empty agent name",
        });
    }

    Ok(AgentBuildSpec {
        profile_name: profile.profile.agent.name.clone(),
        model: profile.profile.agent.model.clone(),
        system_prompt: profile.system_prompt.clone(),
        tools: profile.profile.tools.clone(),
        security: profile.profile.security.clone(),
        context: profile.profile.context.clone(),
        behavior: profile.profile.behavior.clone(),
    })
}
```

### Anti-Patterns to Avoid

- **复制旧 `AgentProfileRaw`:** 它接受 `prompt_path`、`[capabilities]`、`[handoff]`、`[plugins]`、`[orchestrator]`、`[memory]`，这些都是新 profile contract 明确禁止的字段。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- **保留 `load_profile_verified` 的写 sidecar 行为:** 旧 loader 首次加载会写 `.toml.hash`，这和新阶段“只有确认后才创建 default agent 文件”的无惊喜写入原则冲突。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- **让 `lattice-profile` 依赖 runtime/agent/session/plugin:** 违反 D-18，也会让后续删除 orchestrator 更困难。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- **把 coding 行为编码成 typed preset:** 当前 `AgentPreset::Coding` 和 `ToolPreset::Coding` 正是 Phase 1 要从 profile/spec contract 中移除的特殊语义。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: crates/lattice-tools/src/capabilities.rs]

## Extraction Points and Required Refactor Notes

| Current Location | Extract / Replace | Keep | Remove / Reject |
|------------------|-------------------|------|-----------------|
| `crates/lattice-orchestrator/src/profile.rs` | `AgentConfig.name`、`AgentConfig.model`、可选 description、`ToolsConfig.enabled`、prompt-file loading 思路、typed profile errors。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | TOML/serde 解析模式和 package-local prompt-file 校验思路。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | `[capabilities]`、`[handoff]`、`[orchestrator]`、`[memory]`、`[plugins]`、旧 `SystemConfig`、`prompt_path`、`load_profile_verified` hash 写入、`skippable`、`tags`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] |
| `crates/lattice-orchestrator/src/agent_spec.rs` | pure spec/lowering 思路，以及 empty name/model 校验。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] | 编译过渡期如需要，executor trait 可暂留 orchestrator-local。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] | `AgentPreset`、`capabilities.presets` coding alias mapping、`handoff`、`memory`、`project_root`、`cwd`、非必要 `prompt_spec` 依赖。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] |
| `crates/lattice-orchestrator/src/registry.rs` | 扫描子目录 `agent.toml`、记录 failed loads 的模式。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] | `FailedLoad { path, error }` 这类 bulk discovery 结果形状。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] | `load_profile_verified` 和隐式 hash sidecar 生成。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] [VERIFIED: crates/lattice-orchestrator/src/profile.rs] |
| `crates/lattice-orchestrator/src/lattice_dir.rs` | global first、project overlay wins 的 merge 顺序。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] | `.lattice/agents` 和 `~/.lattice/agents` 路径语义。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] | `orchestrator.toml`、`OrchestratorToml`、shared memory DB path。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] |
| `crates/lattice-tools/src/capabilities.rs` | `ToolBundle` enum 名称和 bundle-to-tool expansion。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] | `core`、`workspace`、`filesystem`、`editing`、`shell`、`planning`、`web`。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] | `ToolPreset`、`CapabilitySelection.presets`、`minimal`、`reader`、`coding`、`code-agent` alias。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] |
| `crates/lattice-runtime/src/lib.rs` and `handle.rs` | runtime-owned handle 和 semantic assembly 边界。 [VERIFIED: crates/lattice-runtime/src/lib.rs] [VERIFIED: crates/lattice-runtime/src/handle.rs] | `build_runtime_security`、`resolve_with_provider`、`get_or_create_agent_handle_for_session_seeded`。 [VERIFIED: crates/lattice-runtime/src/lib.rs] | 后续阶段需移除 runtime 对 `lattice_orchestrator::AgentTurnSpec`、plugin overlay、`AgentPreset::Coding`、preset-driven tool assembly 的依赖。 [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| `crates/lattice-cli/src/commands/new_agent.rs` | command 创建 `agents/<name>/agent.toml` 的 package 行为。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs] | 用户可见的 agent package folder 创建。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs] | 模板里的 `[capabilities]`、presets、handoff、`skippable`、`tags`、旧 behavior fields。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs] |
| `crates/lattice-cli/src/coding_agent.rs` | 部分 prompt 文本可在后续成为普通 profile/template 内容。 [VERIFIED: crates/lattice-cli/src/coding_agent.rs] | completion status gating helper 如仍有价值，可迁到 generic runtime CLI code。 [VERIFIED: crates/lattice-cli/src/coding_agent.rs] | specialized coding-agent module、`profile_name = "coding"`、`AgentPreset::Coding`、`capabilities.presets = ["coding"]`。 [VERIFIED: crates/lattice-cli/src/coding_agent.rs] |

## Forbidden Legacy Fields

| Legacy Field / Section | Current Source | New Behavior |
|------------------------|----------------|--------------|
| `[capabilities]` | 当前 profile 接受 `presets` 和 `bundles`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 拒绝并提示改用 `[tools].bundles` 和 `[tools].enabled`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `[handoff]` | 当前 profile 携带 rules、fallback、output schema、`max_turns`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 整段拒绝；generic turn limit 只允许 `behavior.max_turns`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `[orchestrator]` | 当前 profile 携带 subscribe/publish/rpc。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 整段拒绝，因为 bus/orchestrator 不在 v1 scope。 [VERIFIED: .planning/REQUIREMENTS.md] |
| `[plugins]` | 当前 profile 可以加载 plugin DAG slots/edges。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 整段拒绝，因为 plugin 不属于 single-agent core。 [VERIFIED: .planning/REQUIREMENTS.md] |
| `[memory]` | 当前 profile 携带 shared read/write keys。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 整段拒绝；session/memory 行为不是 Phase 1 profile-level contract。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `agent.skippable` | 当前 agent config 包含该字段。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 拒绝，因为 skipping 是 pipeline/orchestration 行为。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `agent.tags` | 当前 agent config 包含 tags。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 拒绝，因为 tags 不属于 v1 minimal profile schema。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `agent.preset` / `agent.type = "code_agent"` | 当前 `AgentConfig` 没有这两个字段，但 context 明确禁止。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] | 拒绝并说明 coding behavior 是 prompt + tools。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `behavior.behavior_type`、`confidence_threshold`、`max_retries`、`enforce_sandbox` | 当前 behavior config 接受这些字段。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 拒绝；只允许 `max_turns`、`reasoning_effort` 等 generic turn controls。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| `prompt_path`、`[system]`、`system.file` | 当前 profile 使用 `[system] prompt/file` 和可选 `prompt_path`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] | 替换为 `[prompt] system = ...` 或 `system_path = ...`，二者必须互斥。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |

## Tool Bundle and Preset Migration Implications

- 当前 `ToolBundle` 值是 `Core`、`Workspace`、`Filesystem`、`Editing`、`Shell`、`Planning`、`Web`；当前 `FromStr` 接受 lowercase 名称 `core`、`workspace`、`filesystem`、`editing`、`shell`、`planning`、`web`。 [VERIFIED: crates/lattice-tools/src/capabilities.rs]
- 当前 `ToolPreset` 值是 `Minimal`、`Reader`、`Coding`，且 `code-agent` 是 `Coding` 的 serde/from-str alias；D-30 要求删除这些 preset path，而不是保留 alias。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- 当前 `CapabilitySelection` 是 `presets: Vec<ToolPreset>` 加 `bundles: Vec<ToolBundle>`；Phase 1 应改成 bundle-only selection 加 explicit enabled tool names。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- 当前测试断言 `coding_preset_resolves_expected_tools`、`reader_preset_excludes_editing_shell_planning_and_web`、`minimal_preset_resolves_no_tools`；这些应改写为 explicit bundle 组合和 explicit enabled tool 的测试。 [VERIFIED: crates/lattice-tools/src/capabilities.rs]
- 当前 CLI/runtime 测试仍使用 `ToolPreset::Coding` 和 `ToolPreset::Reader`；如果在 Phase 1 彻底删 `ToolPreset`，需要同步改写这些测试或保留极短编译桥。 [VERIFIED: crates/lattice-tools/tests/exposure.rs] [VERIFIED: crates/lattice-cli/tests/code_agent_eval.rs]
- 推荐的新 resolver 接口接受 typed bundles 和 enabled names，将 bundle-expanded names 与 explicit tool names 用 `BTreeSet` 合并去重，再过滤 `default_tool_definitions()`。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [ASSUMED]

## Default Agent and Template Behavior

- default 查找顺序是 `.lattice/agents/default/agent.toml`，再 `~/.lattice/agents/default/agent.toml`；该逻辑应落在 `lattice-profile` discovery。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- 如果两个 default 都不存在，`lattice-profile` 应返回 typed `MissingDefault` 或 `DefaultCreationRequired`，包含 project-local 目标路径和 template text；CLI 负责交互确认和写文件。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] [ASSUMED]
- template 必须使用 v1 schema，不能包含 `[capabilities]`、`[handoff]`、`skippable`、`tags`、presets 或旧 behavior fields。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- template 注释必须说明它是自动生成的模板，并提示用户编辑 role、prompt、tools、security settings。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- 当前 `new agent` 模板包含 `[capabilities]`、`presets = ["reader"]`、`[handoff]`、`skippable`、`tags`、旧 behavior fields，因此是重写目标，不是可复用模板。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs]

## Don't Hand-Roll

| Problem | Don't Build | Use Instead | Why |
|---------|-------------|-------------|-----|
| TOML parsing | 手写字符串 section/field parser。 | `toml` + `serde` strict structs；必要时先扫 `toml::Value` 生成更好的 forbidden-field 消息。 | workspace 已标准使用 TOML/Serde；Serde 官方支持 unknown-field rejection。 [VERIFIED: Cargo.toml] [CITED: https://serde.rs/container-attrs.html] |
| Unknown-field rejection | 完全自研递归 TOML parser。 | 每个 schema struct 上用 `#[serde(deny_unknown_fields)]`，再叠加 forbidden-field mapping。 | Serde 负责结构拒绝；pre-scan 只服务更好的用户错误。 [CITED: https://serde.rs/container-attrs.html] |
| Tool expansion | profile crate 展开 bundles 到 tool schemas。 | `lattice-tools` resolver 和 runtime semantic validation。 | D-32 明确 `lattice-profile` 只验证 strings，runtime/tools 验证实际存在性。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| Sandbox policy construction | profile-level Landlock、allowlists、approvals、audit、raw grants。 | runtime 的 `RuntimeSecurityConfig` / `build_runtime_security`。 | 当前 runtime 已验证 `project`、`strict`、`permissive`；Phase 1 只允许 coarse `sandbox_mode`。 [VERIFIED: crates/lattice-runtime/src/lib.rs] |
| Live agent construction | `lattice-profile` 创建 `Agent`、session stores、executors、runtime handles。 | pure lowering 到 data spec，后续由 runtime assembly。 | D-18 禁止 `lattice-profile` 依赖 runtime/session 或构造 live agent。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| Coding behavior | 专门的 coding-agent subtype 或 preset。 | 普通 profile + coding-oriented prompt + explicit tool bundles/enabled tools。 | PROF-03 和 D-25 到 D-30 明确拒绝 coding-specific contract branch。 [VERIFIED: .planning/REQUIREMENTS.md] |

**Key insight:** durable 部分是 declarative profile data 和 pure lowering；旧 orchestration、plugin DAG、handoff、shared memory、coding preset 是本 milestone 要移除的耦合。 [VERIFIED: .planning/PROJECT.md] [VERIFIED: crates/lattice-orchestrator/src/profile.rs]

## Runtime State Inventory

| Category | Items Found | Action Required |
|----------|-------------|-----------------|
| Stored data | scoped audit 未发现 profile 相关数据库/datastore key；这里发现的是 filesystem TOML，不是 SQLite/Redis 等数据。 [VERIFIED: rg scoped audit] | Phase 1 不需要数据库迁移；旧 filesystem profiles 见下方 live/local config。 [VERIFIED: shell `find .lattice/agents "$HOME/.lattice/agents" examples/agents -path '*/agent.toml'`] |
| Live service config | 当前 profile discovery 只基于本地文件系统；repo/env audit 未发现外部 live service config。 [VERIFIED: env `LATTICE_*` audit] [VERIFIED: crates/lattice-orchestrator/src/registry.rs] | 不需要 service API patch；planner 不应假设远程 profile registry。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] |
| OS-registered state | 当前 profile discovery code 不涉及 OS service registration。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] [VERIFIED: crates/lattice-cli/src/commands/mod.rs] | 不需要 OS re-registration task。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] |
| Secrets/env vars | `LATTICE_AGENTS_DIR` 是当前 agents dir env override；当前 shell 未设置任何 `LATTICE_*` 环境变量。 [VERIFIED: crates/lattice-cli/src/commands/mod.rs] [VERIFIED: env audit] | 保留或显式迁移 `LATTICE_AGENTS_DIR`；不要无计划引入新 env var。 [VERIFIED: crates/lattice-cli/src/commands/mod.rs] |
| Build artifacts | 存在 `target/` 和 `Cargo.lock`；scoped audit 未发现 Python egg-info 或 installed package artifact。 [VERIFIED: shell `find . -maxdepth 3`] | 新 crate 测试会更新 build artifacts；不需要手动 artifact 迁移。 [VERIFIED: shell `find . -maxdepth 3`] |
| Existing local/global agent packages | 全局 `~/.lattice/agents/code-review`、`refactor`、`plugin-test` 含 legacy forbidden fields；`examples/agents/local-hello` 和 `superpowers` 也使用 legacy plugin/handoff/tags。 [VERIFIED: shell `find ... agent.toml`] [VERIFIED: shell `rg` legacy-field audit] | 测试不能依赖这些作为有效 v1 profile；后续 UX 应给 actionable error 或在确认后创建新的 default。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |

**Nothing found in category:** 当前 checkout 未发现 project-local `.lattice/agents/*/agent.toml`。 [VERIFIED: shell `find .lattice/agents ...`]

## Common Pitfalls

### Pitfall 1: 无意保留旧 profile 语义

**What goes wrong:** 新 crate 编译通过，但仍带着 `[capabilities]`、`[handoff]`、`[plugins]`、`[orchestrator]`、`[memory]` 或 coding preset 字段。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]  
**Why it happens:** 最容易复制的是 `AgentProfileRaw`，但它是 orchestration profile，不是 v1 generic profile。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]  
**How to avoid:** 先为 D-22 每个字段/section 写 rejection tests，再写 accepted-schema fixture。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]  
**Warning signs:** 测试或新 contract 仍出现 `CapabilitiesConfig.presets`、`HandoffConfig`、`PluginsConfig`、`AgentPreset`、`ToolPreset`。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: crates/lattice-tools/src/capabilities.rs]

### Pitfall 2: `system_path` 仍按 cwd 解析

**What goes wrong:** 同一个 profile 因启动目录不同而行为不同，或路径逃逸 agent package。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]  
**Why it happens:** 旧 `load_prompt_spec` 虽 join profile dir，但 canonical check 用 cwd；旧 `system.file` 也相对 cwd 解析。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]  
**How to avoid:** canonicalize package dir 和 prompt file，要求 prompt path 是文件并且 canonical prompt path starts_with canonical package dir。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] [ASSUMED]  
**Warning signs:** 测试只从 repo root 运行才过，或 `system_path = "../..."` 能读到 package 外部文件。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]

### Pitfall 3: load 阶段写文件

**What goes wrong:** profile discovery/load 意外创建或更新 sidecar 文件。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]  
**Why it happens:** 当前 `load_profile_verified` 首次加载会写 `.toml.hash`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]  
**How to avoid:** 普通 load/discovery 保持 read-only；default template creation 只暴露模板文本和目标路径，CLI 在交互确认后写。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]  
**Warning signs:** `AgentRegistry::load_dir` 或 `ProfileLoader::load` 修改 package directory。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs]

### Pitfall 4: 删除 `ToolPreset` 但没有替换测试意图

**What goes wrong:** preset 删除导致 runtime/CLI 测试挂掉，但新 explicit bundle 行为没有被证明。 [VERIFIED: crates/lattice-tools/tests/exposure.rs] [VERIFIED: crates/lattice-cli/tests/code_agent_eval.rs]  
**Why it happens:** 当前测试通过 `ToolPreset::Coding` 和 `ToolPreset::Reader` 编码产品行为。 [VERIFIED: crates/lattice-tools/tests/exposure.rs]  
**How to avoid:** 改成 `workspace`、`filesystem`、`editing`、`shell`、`planning`、`web` 等 explicit bundle 组合，以及 explicit `enabled` tool names。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]  
**Warning signs:** 新 v1 profile fixture 仍包含 `presets = [...]` 或 `[capabilities]`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

### Pitfall 5: Phase 1 变成 runtime assembly cutover

**What goes wrong:** profile extraction 变成大范围 runtime/CLI/TUI 改写，撞上 Phase 2 和 Phase 3。 [VERIFIED: .planning/ROADMAP.md]  
**Why it happens:** 当前 runtime 导入 `lattice_orchestrator::AgentTurnSpec`、plugin registries、event bus、pipelines、coding preset branches。 [VERIFIED: crates/lattice-runtime/src/lib.rs]  
**How to avoid:** Phase 1 以 compile-safe contract 和必要 bridge 收口；runtime-owned generic agent assembly 放 Phase 2，CLI/TUI command removal 放 Phase 3，除非 plan 明确承担完整测试范围。 [VERIFIED: .planning/ROADMAP.md] [ASSUMED]  
**Warning signs:** 同一 wave 同时改 plugin overlay、pipeline execution、TUI plugin menus、session persistence 和 profile schema。 [VERIFIED: crates/lattice-runtime/src/lib.rs] [VERIFIED: crates/lattice-cli/src/main.rs]

## Code Examples

### Strict Unknown-Field Rejection

```rust
// Source: https://serde.rs/container-attrs.html
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolsSection {
    #[serde(default)]
    bundles: Vec<String>,
    #[serde(default)]
    enabled: Vec<String>,
}
```

Serde 文档说明 `deny_unknown_fields` 会在反序列化遇到 struct 未定义字段时报错；默认情况下，自描述格式里的未知字段可能被忽略。 [CITED: https://serde.rs/container-attrs.html]

### Forbidden-Field Error Shape

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("{path}: forbidden field `{field}` with value `{value}`: {reason}; use {expected}")]
    ForbiddenField {
        path: PathBuf,
        field: &'static str,
        value: String,
        reason: &'static str,
        expected: &'static str,
    },
}
```

D-23 要求错误包含文件路径、section/field/value、拒绝原因和新的 schema shape。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

### Bundle Plus Enabled Tool Union

```rust
pub struct ResolvedTools {
    pub tool_names: std::collections::BTreeSet<String>,
    pub definitions: Vec<lattice_core::types::ToolDefinition>,
}

pub fn resolve_profile_tools(
    bundles: impl IntoIterator<Item = ToolBundle>,
    enabled: impl IntoIterator<Item = String>,
) -> ResolvedTools {
    let mut tool_names = std::collections::BTreeSet::new();
    for bundle in bundles {
        tool_names.extend(bundle.tool_names().into_iter().map(String::from));
    }
    tool_names.extend(enabled);
    let definitions = default_tool_definitions()
        .into_iter()
        .filter(|definition| tool_names.contains(&definition.name))
        .collect();
    ResolvedTools { tool_names, definitions }
}
```

当前 `lattice-tools` 已使用 `BTreeSet` 和 `default_tool_definitions()` 对 bundle-expanded tool names 去重并过滤 definitions。 [VERIFIED: crates/lattice-tools/src/capabilities.rs]

## State of the Art

| Old Approach | Current Approach | When Changed | Impact |
|--------------|------------------|--------------|--------|
| orchestrator-owned `AgentProfile`，包含 plugins、handoff、memory、bus fields。 | dedicated `lattice-profile` crate，只有 strict single-agent config。 | v1.0 Phase 1 planning on 2026-05-14。 [VERIFIED: .planning/ROADMAP.md] | Profile 变成 runtime 输入，而不是 orchestration script。 [VERIFIED: .planning/PROJECT.md] |
| `[capabilities].presets = ["coding" | "reader" | "minimal"]`。 | `[tools].bundles` 和 `[tools].enabled`。 | v1.0 Phase 1 decisions on 2026-05-14。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] | Tool exposure 显式化，不再暗示 agent subtype。 [VERIFIED: .planning/REQUIREMENTS.md] |
| `AgentPreset::Coding` 和 `coding_agent.rs`。 | generic profile prompt + explicit tools。 | v1.0 milestone decisions on 2026-05-14。 [VERIFIED: .planning/REQUIREMENTS.md] | Coding behavior 不再是 runtime/profile type branch。 [VERIFIED: .planning/PROJECT.md] |
| `load_profile_verified` 首次加载写 `.toml.hash`。 | read-only loading；default template 只在确认后创建。 | v1.0 Phase 1 decisions on 2026-05-14。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] | profile load 不应修改用户 package。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |

**Deprecated/outdated:**

- `[system] prompt/file`、`prompt_path`、orchestrator profile 里的 `PromptSpec` loading 对 v1 profile contract 已过时；改用 `[prompt] system` 或 `system_path`。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- `ToolPreset::{Minimal, Reader, Coding}` 和 `CapabilitySelection.presets` 对 v1 profile contract 已过时；改用 explicit bundles 和 enabled tools。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- `AgentPreset::Coding` 对 v1 profile/spec contract 已过时；coding 由 prompt 和 tools 表达。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] [VERIFIED: .planning/REQUIREMENTS.md]

## Assumptions Log

| # | Claim | Section | Risk if Wrong |
|---|-------|---------|---------------|
| A1 | 第一个 v1 schema 用整数 `schema_version = 1`。 [ASSUMED] | Summary / Architecture Patterns | 如果项目偏好 `"1"` 或 `"1.0"` 字符串，fixtures 和错误消息需要调整。 |
| A2 | `lattice-profile` 可以不依赖 `lattice-prompt`，把 `[prompt].system_path` 当普通 system prompt 文本读取。 [ASSUMED] | Standard Stack / Architecture Patterns | 如果 v1 必须保留 `PromptSpec`，crate 依赖边界和 schema 需要单独决策。 |
| A3 | `dirs` 应为可选，因为 caller 可以传 global agents dir。 [ASSUMED] | Standard Stack | 如果 `lattice-profile` 必须直接拥有 home discovery，需要加 `dirs` 并测试 HOME/config-dir 行为。 |
| A4 | 如果完整删除 runtime/CLI preset 超出 Phase 1，先做从旧类型到新 profile 类型的 compile-safe bridge 更稳。 [ASSUMED] | Common Pitfalls | 如果 planner 选择 Phase 1 全量删除，需要更广 runtime/CLI/TUI 测试覆盖。 |
| A5 | tool resolver replacement 用 `BTreeSet` 合并 bundle-expanded 和 explicitly enabled tool names。 [ASSUMED] | Tool Bundle and Preset Migration Implications | 如果 tool order 是用户可见行为，planner 可能需要 ordered set 或确定性 Vec policy。 |

## Open Questions

1. **`schema_version` 的精确表示**
   - What we know: `schema_version` 是允许的顶层字段。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - What's unclear: context 没说明 integer 还是 string。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - Recommendation: 默认用 `schema_version = 1`，除非 planner 想要 semver-style string。 [ASSUMED]

2. **home-directory resolution 归谁**
   - What we know: lookup 必须 project-local before user-global，并保留 `~/.lattice/agents/<name>/agent.toml`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - What's unclear: context 没要求 `lattice-profile` 自己调用 `dirs` 或读取 `HOME`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - Recommendation: 优先设计接受 `project_root` 和 `global_agents_dir` 的 API；只有需要便利函数时再加默认 global path helper。 [ASSUMED]

3. **Phase 1 应删除多少 CLI**
   - What we know: roadmap 把 CLI/TUI single-agent UX 和 `code` 命令删除映射到 Phase 3；但 Phase 1 context 同时拒绝 coding presets 和 code-agent 语义。 [VERIFIED: .planning/ROADMAP.md] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - What's unclear: Phase 1 planner 是否应现在删除 `lattice code`，还是只保证新 profile/spec contract 不能编码它。 [VERIFIED: .planning/ROADMAP.md] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
   - Recommendation: Phase 1 先从新 profile/spec 和 tool presets 移除 code-agent 语义；command-tree UX 删除放 Phase 3，除非编译迫使局部改写。 [ASSUMED]

## Environment Availability

| Dependency | Required By | Available | Version | Fallback |
|------------|-------------|-----------|---------|----------|
| Rust compiler | build/test 新 crate 和 workspace | yes | `rustc 1.95.0` | 无；workspace MSRV 是 1.85。 [VERIFIED: rustc --version] [VERIFIED: Cargo.toml] |
| Cargo | metadata、test、clippy、fmt | yes | `cargo 1.95.0` | 无。 [VERIFIED: cargo --version] |
| Git | status/diff hygiene 和可能的 docs commit | yes | `git 2.54.0` | 无。 [VERIFIED: git --version] |
| ripgrep | boundary grep checks 和 source audits | yes | `ripgrep 15.1.0` | 不可用时用 `grep`。 [VERIFIED: rg --version] |
| jq | metadata inspection | yes | 通过 `cargo metadata ... | jq` 成功确认 | 不可用时读 raw JSON 或 cargo metadata output。 [VERIFIED: cargo metadata command] |

**Missing dependencies with no fallback:** Phase 1 research/planning 未发现。 [VERIFIED: environment audit]

**Missing dependencies with fallback:** 无必需项；`jq` 只是便利工具。 [VERIFIED: environment audit]

## Validation Architecture

### Test Framework

| Property | Value |
|----------|-------|
| Framework | Cargo built-in Rust test runner；需要 async 时使用 `#[tokio::test]`。 [VERIFIED: .planning/codebase/TESTING.md] |
| Config file | 没有专用 Rust test config；root `Cargo.toml` 控制 workspace 和依赖。 [VERIFIED: Cargo.toml] |
| Quick run command | 新 crate 存在后运行 `cargo test -p lattice-profile`。 [VERIFIED: .planning/codebase/TESTING.md] [ASSUMED] |
| Full suite command | `cargo test --workspace`；高风险 refactor 还要跑 `cargo fmt --all --check` 和 `cargo clippy --workspace -- -D warnings`。 [VERIFIED: .planning/codebase/TESTING.md] [VERIFIED: AGENTS.md] |

### Phase Requirements -> Test Map

| Req ID | Behavior | Test Type | Automated Command | File Exists? |
|--------|----------|-----------|-------------------|--------------|
| PROF-01 | 有效 v1 `agent.toml` 可解析；forbidden legacy sections/fields 给出 actionable errors。 | unit/parser | `cargo test -p lattice-profile strict_schema` | No - Wave 0 创建 `crates/lattice-profile`。 [VERIFIED: .planning/REQUIREMENTS.md] |
| PROF-01 | `prompt.system` 和 `prompt.system_path` 互斥；`system_path` 必须存在、是文件、位于 package 内。 | unit/filesystem | `cargo test -p lattice-profile prompt_path` | No - Wave 0。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| PROF-01 | explicit path、named lookup、project-over-global override、default-agent lookup order 正确。 | unit/filesystem | `cargo test -p lattice-profile discovery` | No - Wave 0。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs] |
| PROF-02 | lowering 只复制 loaded profile data 到 pure spec，不依赖 runtime/agent/session/plugin，不读文件。 | unit + source guard | `cargo test -p lattice-profile lowering` | No - Wave 0。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs] |
| PROF-03 | coding-capable profile 通过 prompt + explicit tools 表达；v1 fixtures 不含 `AgentPreset::Coding`、`ToolPreset::Coding`、`[capabilities]`。 | unit + grep guard | `cargo test -p lattice-tools capabilities && rg "AgentPreset::Coding|ToolPreset::Coding|\\[capabilities\\]" crates/lattice-profile crates/lattice-tools` | `lattice-tools` 存在；`lattice-profile` No - Wave 0。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] |

### Sampling Rate

- **Per task commit:** `cargo test -p lattice-profile`，以及直接改动 crate 的定向测试，例如 `cargo test -p lattice-tools capabilities`。 [VERIFIED: .planning/codebase/TESTING.md]
- **Per wave merge:** `cargo test -p lattice-profile`、`cargo test -p lattice-tools`；如果 touched runtime spec types，再跑 `cargo test -p lattice-runtime`；同时跑 `cargo fmt --all --check`。 [VERIFIED: .planning/codebase/TESTING.md] [ASSUMED]
- **Phase gate:** `cargo fmt --all --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace`；如遇无关既有失败，记录并提供 narrow passing gates。 [VERIFIED: AGENTS.md] [VERIFIED: .planning/codebase/TESTING.md]

### Wave 0 Gaps

- [ ] `crates/lattice-profile/Cargo.toml` - 新 crate manifest 和 workspace dependency declarations。 [VERIFIED: Cargo.toml]
- [ ] `crates/lattice-profile/src/lib.rs` - public profile contract exports。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- [ ] `crates/lattice-profile/src/schema.rs` 或等价模块 - strict serde schema with `deny_unknown_fields`。 [CITED: https://serde.rs/container-attrs.html]
- [ ] `crates/lattice-profile/src/loader.rs` 或等价模块 - strict load、forbidden-field scanner、package-local prompt path checks。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs]
- [ ] `crates/lattice-profile/src/discovery.rs` 或等价模块 - explicit/name/default project/global lookup。 [VERIFIED: crates/lattice-orchestrator/src/lattice_dir.rs]
- [ ] `crates/lattice-profile/src/template.rs` 或等价模块 - v1 default template。 [VERIFIED: crates/lattice-cli/src/commands/new_agent.rs]
- [ ] `crates/lattice-profile/src/lowering.rs` 或等价模块 - pure spec conversion。 [VERIFIED: crates/lattice-orchestrator/src/agent_spec.rs]
- [ ] `crates/lattice-profile` source guard - 如果 import `lattice_runtime`、`lattice_orchestrator`、`lattice_agent`、`lattice_session`、`lattice_plugin` 或 CLI/TUI modules，测试失败。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]

## Security Domain

### Applicable ASVS Categories

| ASVS Category | Applies | Standard Control |
|---------------|---------|------------------|
| V2 Authentication | no | Profile parsing 不做用户或 provider 认证；runtime/model credentials 不在本 phase。 [VERIFIED: .planning/REQUIREMENTS.md] |
| V3 Session Management | no | Profile lowering 不读写 session stores；session persistence 属于 runtime/session 工作。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| V4 Access Control | yes | Tool exposure 通过 `[tools]` allowlist 和 runtime/tool semantic validation；profile 只保存 strings。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| V5 Input Validation | yes | strict TOML schema、forbidden-field rejection、enum/string validation、package-local path checks。 [CITED: https://serde.rs/container-attrs.html] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| V6 Cryptography | limited | 不引入自研 crypto；旧 blake3 sidecar 行为存在，但除非明确计划，否则不要复制。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [ASSUMED] |

### Known Threat Patterns for Rust TOML Profile Loading

| Pattern | STRIDE | Standard Mitigation |
|---------|--------|---------------------|
| `prompt.system_path` 路径穿越或 symlink escape | Tampering / Information Disclosure | canonicalize package dir 和 prompt file；要求 prompt path 位于 package 内且是文件。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] [ASSUMED] |
| 未知 legacy section 被静默接受 | Tampering / Elevation of Privilege | `#[serde(deny_unknown_fields)]` 加 explicit forbidden-field scanner。 [CITED: https://serde.rs/container-attrs.html] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| profile 直接授予 sandbox allowlists 或 approvals | Elevation of Privilege | 拒绝 profile-level approval mode、allowlists、Landlock、audit、raw grants；只允许 coarse `sandbox_mode`。 [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| tool preset 意外扩成过宽 capability | Elevation of Privilege | 删除 presets，要求 explicit bundles/enabled tool names，并由 runtime/tools 验证。 [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |
| loader 在 discovery 中修改 profile package | Tampering | 普通 profile load read-only；default 文件写入必须经过 interactive confirmation。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md] |

## Sources

### Primary (HIGH confidence)

- `.planning/phases/01-profile-contract-extraction/01-CONTEXT.md` - Phase 1 locked decisions、scope、forbidden fields、crate boundary、default behavior。 [VERIFIED: local file read]
- `.planning/REQUIREMENTS.md` - PROF-01、PROF-02、PROF-03 和 milestone constraints。 [VERIFIED: local file read]
- `.planning/ROADMAP.md` - Phase 1 success criteria 和 Phase 2/3 分界。 [VERIFIED: local file read]
- `.planning/STATE.md` - 当前 milestone state 和 accumulated decisions。 [VERIFIED: local file read]
- `AGENTS.md` 和 `CLAUDE.md` - Rust workspace gates 和 dependency flow。 [VERIFIED: local file read]
- `crates/lattice-orchestrator/src/profile.rs` - 旧 profile schema、loader、prompt path、hash sidecar、legacy fields。 [VERIFIED: local file read]
- `crates/lattice-orchestrator/src/agent_spec.rs` - 当前 pure-ish spec/lowering 和 `AgentPreset::Coding`。 [VERIFIED: local file read]
- `crates/lattice-orchestrator/src/registry.rs` 和 `lattice_dir.rs` - 当前 agent directory scanning 和 project/global merge behavior。 [VERIFIED: local file read]
- `crates/lattice-tools/src/capabilities.rs` - 当前 bundles、presets、resolver、tests。 [VERIFIED: local file read]
- `crates/lattice-runtime/src/lib.rs`、`handle.rs`、`preset.rs` - runtime security modes、handle/spec usage、coding-preset coupling。 [VERIFIED: local file read]
- `crates/lattice-cli/src/commands/run.rs`、`new_agent.rs`、`main.rs`、`security.rs`、`coding_agent.rs` - 当前 CLI run/code/default-template/security behavior。 [VERIFIED: local file read]
- `.planning/codebase/STACK.md`、`CONVENTIONS.md`、`TESTING.md` - workspace conventions、test matrix、stack map。 [VERIFIED: local file read]
- `Cargo.toml`、`Cargo.lock`、`cargo metadata` - workspace members、dependency constraints、locked versions。 [VERIFIED: local commands]
- Serde docs via Context7 `/websites/serde_rs` - `deny_unknown_fields` 行为。 [CITED: https://serde.rs/container-attrs.html]

### Secondary (MEDIUM confidence)

- 本地 filesystem audit：`.lattice/agents`、`~/.lattice/agents`、`examples/agents` - 现有 legacy profile 文件和 forbidden-field examples。 [VERIFIED: shell `find` and `rg`]
- 环境 audit：`rustc`、`cargo`、`git`、`rg`、`jq`、`LATTICE_*` variables。 [VERIFIED: shell commands]

### Tertiary (LOW confidence)

- None。 [VERIFIED: research process]

## Metadata

**Confidence breakdown:**

- Standard stack: HIGH - 版本和 crate boundaries 已由 workspace manifests/lockfile 和 Phase 1 context 核验。 [VERIFIED: Cargo.toml] [VERIFIED: Cargo.lock] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- Architecture: HIGH - extraction points 和 dependency boundaries 已由当前 source、roadmap、context 核验。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: crates/lattice-runtime/src/lib.rs] [VERIFIED: .planning/ROADMAP.md]
- Pitfalls: HIGH - 风险直接对应当前代码路径和 locked forbidden concepts。 [VERIFIED: crates/lattice-orchestrator/src/profile.rs] [VERIFIED: crates/lattice-tools/src/capabilities.rs] [VERIFIED: .planning/phases/01-profile-contract-extraction/01-CONTEXT.md]
- Runtime state: MEDIUM - 已 audit local/global profile 文件和 env vars；未查外部 service registry，因为当前 profile discovery 是 local-filesystem based。 [VERIFIED: crates/lattice-orchestrator/src/registry.rs] [VERIFIED: shell audit]

**Research date:** 2026-05-14  
**Valid until:** 2026-05-28 for this active refactor area。 [ASSUMED]
