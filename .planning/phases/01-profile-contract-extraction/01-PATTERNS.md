# Phase 1: Profile Contract Extraction - Pattern Map

**Mapped:** 2026-05-14
**Files analyzed:** 12 个新建/修改目标
**Analogs found:** 11 / 12
**范围说明:** 本阶段只抽取 profile contract。旧 `lattice-orchestrator` 代码是来源和反模式集合，不应把 plugin/handoff/orchestrator/subagent/coding preset 语义带入 `lattice-profile`。

## File Classification

| New/Modified File | Role | Data Flow | Closest Analog | Match Quality |
|---|---|---|---|---|
| `Cargo.toml` | config | config | `Cargo.toml` workspace members/dependencies | exact |
| `crates/lattice-profile/Cargo.toml` | config | config | `crates/lattice-orchestrator/Cargo.toml` + narrow crate manifests | role-match |
| `crates/lattice-profile/src/lib.rs` | provider/config API | transform | `crates/lattice-orchestrator/src/lib.rs` export style, `agent_spec.rs` guardrail | role-match |
| `crates/lattice-profile/src/schema.rs` | model | transform | `crates/lattice-orchestrator/src/profile.rs` | role-match, semantics must be narrowed |
| `crates/lattice-profile/src/error.rs` | utility | request-response | `crates/lattice-plugin/src/manifest.rs` `ManifestError` | role-match |
| `crates/lattice-profile/src/load.rs` | service | file-I/O | `profile.rs::AgentProfile::load`, `manifest.rs::load_manifest` | exact for I/O pattern |
| `crates/lattice-profile/src/discovery.rs` | service | file-I/O | `registry.rs` + `lattice_dir.rs` + CLI `safe_agents_dir` | exact |
| `crates/lattice-profile/src/defaults.rs` | utility | file-I/O/template | `crates/lattice-cli/src/commands/new_agent.rs` | role-match, template must be rewritten |
| `crates/lattice-profile/src/spec.rs` | model | transform | `crates/lattice-orchestrator/src/agent_spec.rs` | role-match, remove preset/handoff/memory |
| `crates/lattice-profile/src/lowering.rs` | utility/service | transform | `agent_spec.rs::lower_profile_to_turn_spec` | exact for purity, semantics must be narrowed |
| `crates/lattice-tools/src/capabilities.rs` | utility/service | transform | same file | exact, remove preset branch |
| `crates/lattice-cli/src/commands/new_agent.rs` | controller/command | file-I/O + request-response | same file + `commands/mod.rs::safe_agents_dir` | exact for command shell, template rewrite |

## Pattern Assignments

### `Cargo.toml` (config, config)

**Analog:** root `Cargo.toml`

**Workspace member/dependency pattern** (lines 1-40):

```toml
[workspace]
resolver = "2"
members = [
    "crates/lattice-hooks",
    "crates/lattice-core",
    "crates/lattice-session",
    "crates/lattice-prompt",
    "crates/lattice-agent",
    "crates/lattice-plugin",
    "crates/lattice-tools",
    "crates/lattice-orchestrator",
    "crates/lattice-runtime",
    "crates/lattice-plugins",
    "crates/lattice-cli",
    "crates/lattice-sandbox",
    "crates/lattice-policy",
]

[workspace.dependencies]
lattice-hooks    = { path = "crates/lattice-hooks" }
lattice-core     = { path = "crates/lattice-core" }
...
lattice-policy   = { path = "crates/lattice-policy" }
```

**Apply:** add `"crates/lattice-profile"` as a member and `lattice-profile = { path = "crates/lattice-profile" }` under internal workspace dependencies. Keep existing formatting/alignment style.

**Boundary rule:** `lattice-profile` may depend on low crates needed for data contracts, but must not depend on `lattice-runtime`, `lattice-orchestrator`, `lattice-agent`, `lattice-session`, `lattice-plugin`, or CLI/TUI state.

---

### `crates/lattice-profile/Cargo.toml` (config, config)

**Analog:** `crates/lattice-orchestrator/Cargo.toml` and narrow crate manifests

**Package/dependency pattern** (`crates/lattice-orchestrator/Cargo.toml` lines 1-33):

```toml
[package]
name = "lattice-orchestrator"
version.workspace = true
license.workspace = true
edition.workspace = true
authors.workspace = true
description = "Orchestrator, pipeline orchestration, and handoff rules"

[dependencies]
lattice-core = { workspace = true }
lattice-agent = { workspace = true }
lattice-hooks = { workspace = true }
lattice-prompt = { workspace = true }
lattice-tools = { workspace = true }
lattice-plugin = { workspace = true }
...
toml = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

**Use but narrow:** expected `lattice-profile` dependencies should be closer to:

```toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

Only add `lattice-prompt` if the planner decides `AgentProfileSpec` must embed `PromptSpec`. Prefer a profile-owned pure prompt representation if that keeps the crate narrower.

---

### `crates/lattice-profile/src/schema.rs` (model, transform)

**Analog:** `crates/lattice-orchestrator/src/profile.rs`

**Imports pattern** (lines 1-2):

```rust
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
```

For the new schema, split path-heavy loading code into `load.rs`; `schema.rs` should mostly need serde and small enums.

**Legacy schema source to extract from** (lines 37-59):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentProfile {
    pub agent: AgentConfig,
    pub system: SystemConfig,
    #[serde(default)]
    pub capabilities: CapabilitiesConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub behavior: BehaviorConfig,
    #[serde(default)]
    pub handoff: HandoffConfig,
    #[serde(default)]
    pub orchestrator: OrchestratorConfigProfile,
    #[serde(default)]
    pub memory: MemoryConfigProfile,
    #[serde(skip)]
    pub plugins: Option<PluginsConfig>,
    #[serde(skip)]
    pub prompt_spec: Option<lattice_prompt::PromptSpec>,
}
```

**Do not copy these fields:** `capabilities`, `handoff`, `orchestrator`, `memory`, `plugins`, `prompt_spec` as computed plugin/prompt-spec state. New top-level allowlist is only `schema_version`, `[agent]`, `[prompt]`, `[tools]`, `[security]`, `[context]`, `[behavior]`.

**Agent identity pattern, narrowed** (old lines 61-71):

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub skippable: bool,
    #[serde(default)]
    pub tags: Vec<String>,
}
```

**New rule:** keep `name`, `model`, optional `description`; reject `preset`, `type = "code_agent"`, `skippable`, and `tags` through strict serde / validation tests.

**Tools pattern, narrowed** (old lines 96-100 plus context decisions):

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub enabled: Vec<String>,
}
```

**New rule:** move bundles into `[tools]` and keep both arrays as strings:

```rust
pub struct ToolsConfig {
    pub bundles: Vec<String>,
    pub enabled: Vec<String>,
}
```

`lattice-profile` validates shape only. Actual bundle/tool existence belongs to `lattice-tools`/runtime.

**Strictness requirement:** repository search found no current production `#[serde(deny_unknown_fields)]` analog. Use serde strictness deliberately on the new v1 TOML structs and cover it with tests. This is a new pattern, not copied from old profile code.

---

### `crates/lattice-profile/src/error.rs` (utility, request-response)

**Analog:** `crates/lattice-plugin/src/manifest.rs::ManifestError`

**Typed error with path/source pattern** (lines 9-25):

```rust
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error at {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("plugin manifest at {path} is invalid: {message}")]
    Invalid { path: PathBuf, message: String },
}
```

**Apply:** define `ProfileError` variants with file path and concrete field/section/value information. Avoid old `Box<dyn std::error::Error>` return style from `profile.rs::load`.

**Old profile error pattern to avoid** (`profile.rs` lines 11-31):

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("prompt spec error: {0}")]
    PromptSpec(String),
    ...
}
```

The new phase needs more actionable messages than this: include `path`, `section`, `field`, rejected `value`, and suggested v1 shape where practical.

---

### `crates/lattice-profile/src/load.rs` (service, file-I/O)

**Analogs:** `profile.rs::AgentProfile::load`, `manifest.rs::load_manifest`, `manifest.rs::safe_relative_path`

**TOML read + parse pattern** (`manifest.rs` lines 174-183):

```rust
pub fn load_manifest(path: &Path) -> Result<PluginBundle, ManifestError> {
    let content = std::fs::read_to_string(path).map_err(|source| ManifestError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let raw: PluginToml = toml::from_str(&content).map_err(|source| ManifestError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
```

**Inline-or-file exclusivity pattern** (`manifest.rs` lines 276-294):

```rust
fn read_inline_or_file(
    manifest_path: &Path,
    base_dir: &Path,
    inline: String,
    file: Option<&str>,
    field: &str,
) -> Result<String, ManifestError> {
    match file {
        Some(_) if !inline.is_empty() => Err(ManifestError::Invalid {
            path: manifest_path.to_path_buf(),
            message: format!("use inline {field} content or file reference, not both"),
        }),
        Some(file) => {
            let path = safe_relative_path(manifest_path, base_dir, file, field)?;
            std::fs::read_to_string(&path).map_err(|source| ManifestError::Io { path, source })
        }
        None => Ok(inline),
    }
}
```

**Relative path validation pattern** (`manifest.rs` lines 296-310):

```rust
fn safe_relative_path(
    manifest_path: &Path,
    base_dir: &Path,
    relative: &str,
    field: &str,
) -> Result<PathBuf, ManifestError> {
    let path = Path::new(relative);
    if path.is_absolute() || relative.contains("..") {
        return Err(ManifestError::Invalid {
            path: manifest_path.to_path_buf(),
            message: format!("{field} must be a relative path without '..'"),
        });
    }
    Ok(base_dir.join(path))
}
```

**Strengthen for Phase 1:** `prompt.system_path` must resolve relative to the `agent.toml` directory, must exist, must be a file, and must not escape the agent package directory. The old `profile.rs::load_prompt_spec` validates relative paths but compares against cwd (lines 304-315); new code should compare canonical path against canonical package directory.

**Old cwd-based check to replace** (`profile.rs` lines 304-315):

```rust
let resolved = profile_dir.join(prompt_path);
let canonical = std::fs::canonicalize(&resolved)
    .map_err(|e| format!("prompt_path '{}' not found: {e}", prompt_path))?;
if let Ok(cwd) = std::env::current_dir() {
    if !canonical.starts_with(&cwd) {
        return Err(format!(
            "prompt_path '{}' resolved outside working directory",
            prompt_path
        )
        .into());
    }
}
```

---

### `crates/lattice-profile/src/discovery.rs` (service, file-I/O)

**Analogs:** `registry.rs`, `lattice_dir.rs`, CLI `commands/mod.rs`

**Folder scan pattern** (`registry.rs` lines 38-73):

```rust
pub fn load_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
    let mut registry = Self::empty();

    if !dir.exists() {
        return Ok(registry);
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let agent_toml = entry.path().join("agent.toml");
            if agent_toml.exists() {
                match crate::profile::load_profile_verified(&agent_toml) {
                    Ok((profile, _hash)) => {
                        let name = profile.agent.name.clone();
                        registry.agents.insert(name, profile);
                    }
                    Err(e) => {
                        registry.failed_loads.push(FailedLoad {
                            path: agent_toml.clone(),
                            error: e.to_string(),
                        });
                        tracing::warn!("failed to load agent at {}: {}", agent_toml.display(), e);
                    }
                }
            }
        }
    }

    Ok(registry)
}
```

**Apply with changes:** keep `agents/<name>/agent.toml`, failed-load accumulation, and non-existent directory returning empty. Do not use `load_profile_verified` or `.toml.hash` sidecars in v1 unless explicitly planned; Phase 1 did not decide integrity sidecars.

**Project-over-global merge pattern** (`lattice_dir.rs` lines 108-140):

```rust
/// Combine project-local agents with global ~/.lattice/agents/.
/// Project-local agents take precedence (same name overrides global).
pub fn discover_with_global(
    project_root: &Path,
    global_dir: &Path,
) -> Result<Self, Box<dyn std::error::Error>> {
    let lattice_dir = project_root.join(".lattice");
    if !lattice_dir.exists() {
        let registry = AgentRegistry::load_dir(global_dir)?;
        let orchestrator_config = OrchestratorToml::default();
        return Ok(Self { root: global_dir.to_path_buf(), orchestrator_config, registry });
    }

    let global_registry = AgentRegistry::load_dir(global_dir)?;
    let project_agents_dir = lattice_dir.join("agents");
    let project_registry = AgentRegistry::load_dir(&project_agents_dir)?;
    let merged = global_registry.merge(project_registry);
```

**Apply:** preserve lookup priority exactly: explicit file path first; named lookup checks `.lattice/agents/<name>/agent.toml` before `~/.lattice/agents/<name>/agent.toml`; default lookup uses name `default` with the same priority.

**Path safety pattern** (`crates/lattice-cli/src/commands/mod.rs` lines 18-61):

```rust
pub fn safe_agents_dir(override_path: Option<&str>) -> Result<PathBuf, String> {
    let path = if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    };

    validate_agents_dir(&path)?;
    Ok(path)
}
```

Use this as CLI-facing precedent. In `lattice-profile`, prefer accepting explicit `project_root` / `global_agents_dir` paths from the caller instead of reading CLI env by default, unless the API is specifically a convenience helper.

---

### `crates/lattice-profile/src/defaults.rs` (utility, file-I/O/template)

**Analog:** `crates/lattice-cli/src/commands/new_agent.rs`

**Current template pattern** (lines 5-40):

```rust
const TEMPLATE: &str = r#"# LATTICE Agent Profile
# Generated by `lattice new agent`

[agent]
name = "AGENT_NAME"
model = "{MODEL}"
skippable = false
tags = []

[system]
prompt = "You are a helpful assistant. Return JSON."

[capabilities]
presets = ["reader"]
bundles = []
...
"#;
```

**Do not copy old schema:** remove `[capabilities]`, `presets`, `[system]`, `[handoff]`, `skippable`, `tags`, `behavior_type`, `confidence_threshold`, and `max_retries` if the new `[behavior]` only carries `max_turns` and `reasoning_effort`.

**Command write pattern** (lines 42-70):

```rust
pub fn run(name: String) -> Result<()> {
    let agents_dir = super::safe_agents_dir(None).map_err(anyhow::Error::msg)?;
    let dir = agents_dir.join(&name);

    if dir.exists() {
        bail!("Agent '{}' already exists at '{}'", name, dir.display());
    }

    std::fs::create_dir_all(&dir)?;
    let config = Config::load(None)?;
    let default_model = config.default_model();
    let template = TEMPLATE
        .replace("AGENT_NAME", &name)
        .replace("{MODEL}", &default_model);
    let toml_path = dir.join("agent.toml");
    std::fs::write(&toml_path, template)?;
```

**Apply:** move the new v1 template text into `lattice-profile` so CLI can call it. The generated default agent template must include comments saying it is auto-generated and should be edited for project role, prompt, tools, and security settings.

**Interactive default creation rule:** no direct analog found for “prompt before creating default agent”. Planner should add an API boundary such as `DefaultAgentPolicy::{Create, ErrorIfMissing}` or keep prompting in CLI and pure template generation in `lattice-profile`.

---

### `crates/lattice-profile/src/spec.rs` (model, transform)

**Analog:** `crates/lattice-orchestrator/src/agent_spec.rs`

**Pure data contract pattern** (lines 1-8, 33-68):

```rust
//! Lower-level agent turn contract for runtime-owned execution.
//!
//! This module defines pure data types (spec, request, output) and an
//! executor trait that `lattice-runtime` can implement. It intentionally
//! does NOT import `lattice_runtime` or construct live `lattice_agent::Agent`,
//! so the orchestrator-to-runtime dependency direction stays valid.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnSpec {
    pub profile_name: String,
    pub model: String,
    pub provider_override: Option<String>,
    pub system_prompt: Option<String>,
    pub prompt_spec: Option<lattice_prompt::PromptSpec>,
    pub capabilities: crate::profile::CapabilitiesConfig,
    pub tools: crate::profile::ToolsConfig,
    pub behavior: crate::profile::BehaviorConfig,
    pub project_root: Option<String>,
    pub cwd: Option<String>,
    pub thinking_effort: Option<String>,
    pub preset: Option<AgentPreset>,
}
```

**Apply with narrowed fields:** the new spec should not include `AgentPreset`, `handoff`, `orchestrator`, `memory`, `plugins`, `project_root`, or `cwd` from profile. Runtime invocation context supplies cwd/project root. Suggested fields are pure profile-derived data: `profile_name`, `model`, optional `description`, resolved `system_prompt`, `tools { bundles, enabled }`, `security.sandbox_mode`, `context` toggles, and `behavior` turn controls.

**Request/output/executor trait:** Phase 1 does not need to copy the executor trait unless planner wants a test fake. The important reusable pattern is the pure-data separation and no runtime import.

---

### `crates/lattice-profile/src/lowering.rs` (utility/service, transform)

**Analog:** `agent_spec.rs::lower_profile_to_turn_spec`

**Lowering purity pattern** (lines 179-196):

```rust
/// Lower an `AgentProfile` into an executable `AgentTurnSpec`.
///
/// This function copies profile fields into a pure-data spec. It does NOT
/// resolve models, construct executors, create live `Agent` instances,
/// or read the filesystem.
pub fn lower_profile_to_turn_spec(
    profile: &AgentProfile,
) -> Result<AgentTurnSpec, ProfileLoweringError> {
```

**Required field checks** (lines 197-203):

```rust
if profile.agent.name.is_empty() {
    return Err(ProfileLoweringError::EmptyProfileName);
}
if profile.agent.model.is_empty() {
    return Err(ProfileLoweringError::EmptyModel);
}
```

**Do not copy preset derivation** (lines 205-215):

```rust
let preset = if profile
    .capabilities
    .presets
    .iter()
    .any(|p| p == "coding" || p == "code-agent")
{
    Some(AgentPreset::Coding)
} else {
    None
};
```

**Copy field-mapping style only** (lines 235-250):

```rust
Ok(AgentTurnSpec {
    profile_name: profile.agent.name.clone(),
    model: profile.agent.model.clone(),
    provider_override: None,
    system_prompt,
    prompt_spec: profile.prompt_spec.clone(),
    capabilities: profile.capabilities.clone(),
    tools: profile.tools.clone(),
    behavior: profile.behavior.clone(),
    project_root: None,
    cwd: None,
    thinking_effort: None,
    preset,
})
```

**Apply:** lowering must not resolve provider aliases, check credentials, expand tool bundles, construct sandbox, read session stores, create agents, or infer coding presets. It can validate empty strings and mutually exclusive prompt fields already loaded into memory.

**Guardrail test pattern** (lines 411-438):

```rust
#[cfg(test)]
mod lower_contract_tests {
    #[test]
    fn orchestrator_lower_contract_exposes_no_live_agent() {
        let source = include_str!("agent_spec.rs");
        let prod_end = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod_source = &source[..prod_end];
        for line in prod_source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("use ") && trimmed.contains("lattice_runtime") {
                panic!("agent_spec.rs must not import lattice_runtime: {trimmed}");
            }
        }
    }
}
```

Add similar source-level tests for forbidden imports/symbols in `lattice-profile`: `lattice_runtime`, `lattice_orchestrator`, `lattice_agent`, `lattice_session`, `lattice_plugin`, `AgentPreset`, `code_agent`, `handoff`, `orchestrator`, `plugins`, `subagents`.

---

### `crates/lattice-tools/src/capabilities.rs` (utility/service, transform)

**Analog:** same file

**Bundle enum pattern to keep** (lines 15-25):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolBundle {
    Core,
    Workspace,
    Filesystem,
    Editing,
    Shell,
    Planning,
    Web,
}
```

**Preset pattern to remove** (lines 27-42, 97-112, 165-176):

```rust
pub enum ToolPreset {
    Minimal,
    Reader,
    #[serde(alias = "code-agent")]
    Coding,
}

pub struct CapabilitySelection {
    #[serde(default)]
    pub presets: Vec<ToolPreset>,
    #[serde(default)]
    pub bundles: Vec<ToolBundle>,
}
```

**Bundle expansion pattern to keep** (lines 114-145):

```rust
impl ToolBundle {
    pub(crate) fn tool_names(self) -> Vec<&'static str> {
        match self {
            ToolBundle::Core => { ... }
            ToolBundle::Workspace => vec!["workspace_context"],
            ToolBundle::Filesystem => vec![
                "read_file",
                "read_files",
                "grep",
                "list_directory",
                "find_files",
            ],
            ToolBundle::Editing => vec!["write_file", "patch"],
            ToolBundle::Shell => vec!["bash"],
            ToolBundle::Planning => vec!["todo_write"],
            ToolBundle::Web => vec!["web_search"],
        }
    }
}
```

**Runtime semantic validation pattern** (`RuntimeSecurity::approve_rule`, `crates/lattice-runtime/src/lib.rs` lines 108-116):

```rust
if let ApprovalKind::Bundle { bundle_name } = &rule.kind {
    ToolBundle::from_str(bundle_name)
        .map_err(|_| ValidationError::UnknownBundle(bundle_name.clone()))?;
}
```

**Apply:** `lattice-profile` stores bundle names as strings; `lattice-tools` should own `ToolBundle::from_str` and resolution. If `CapabilitySelection` grows `enabled: Vec<String>`, union/dedup should use `BTreeSet` like current `ResolvedCapabilities` (lines 44-49, 59-75).

---

### `crates/lattice-cli/src/commands/new_agent.rs` (controller/command, file-I/O + request-response)

**Analog:** same file

**Command skeleton to keep** (lines 42-70):

```rust
pub fn run(name: String) -> Result<()> {
    let agents_dir = super::safe_agents_dir(None).map_err(anyhow::Error::msg)?;
    let dir = agents_dir.join(&name);

    if dir.exists() {
        bail!("Agent '{}' already exists at '{}'", name, dir.display());
    }

    std::fs::create_dir_all(&dir)?;
    let config = Config::load(None)?;
    let default_model = config.default_model();
    let template = TEMPLATE
        .replace("AGENT_NAME", &name)
        .replace("{MODEL}", &default_model);
    let toml_path = dir.join("agent.toml");
    std::fs::write(&toml_path, template)?;
```

**Current test to replace** (lines 73-81):

```rust
#[cfg(test)]
mod tests {
    use super::TEMPLATE;

    #[test]
    fn template_declares_reader_capabilities() {
        assert!(TEMPLATE.contains("[capabilities]"));
        assert!(TEMPLATE.contains("presets = [\"reader\"]"));
    }
}
```

**Apply:** tests should assert the new template contains `[prompt]`, `[tools]`, `[security]`, `[context]`, `[behavior]`; does not contain `[capabilities]`, `presets`, `[handoff]`, `skippable`, `tags`, `type = "code_agent"`, or coding preset strings.

---

### `crates/lattice-runtime/src/lib.rs` (service, request-response/streaming) - semantic boundary reference

**Analog role:** not necessarily a Phase 1 primary target unless planner removes `ToolPreset` now and must keep workspace compiling. Use this file to preserve boundary language.

**Security mode contract** (lines 190-237):

```rust
let mut sandbox = match config.sandbox_mode.as_str() {
    "permissive" => SandboxConfig::permissive(),
    "project" | "strict" => SandboxConfig {
        read_allowlist: if config.read_allowlist.is_empty() {
            vec![canonical_workdir.clone()]
        } else {
            config.read_allowlist.clone()
        },
        write_allowlist: if config.write_allowlist.is_empty() {
            vec![canonical_workdir.clone()]
        } else {
            config.write_allowlist.clone()
        },
        ...
    },
    mode => {
        return Err(RuntimeError::Config(format!(
            "unknown security.sandbox_mode '{}': expected 'project', 'strict', or 'permissive'",
            mode
        )));
    }
};
```

**Apply:** profile schema may allow only `project`, `strict`, `permissive`, but runtime remains the owner of actual `SandboxConfig` construction and allowlists.

**Current AgentSpec preset code to avoid in new profile contract** (lines 407-435):

```rust
pub fn preset(mut self, preset: AgentPreset) -> Self {
    self.preset = Some(preset);
    self
}

pub fn coding_preset(self) -> Self {
    self.preset(AgentPreset::Coding)
}

pub fn capability_preset(mut self, preset: impl Into<String>) -> Self {
    let preset = preset.into();
    match ToolPreset::from_str(&preset) {
        Ok(preset) => self.capabilities.presets.push(preset),
        Err(_) => tracing::warn!("unknown capability preset '{}' on AgentSpec", preset),
    }
    self
}
```

**Boundary:** do not import this preset model into `lattice-profile`. If `lattice-tools` removes presets in Phase 1, planner must update runtime compile sites and tests around lines 1917-1964 that assert preset behavior.

## Shared Patterns

### TOML Config Loading

**Sources:** `crates/lattice-orchestrator/src/profile.rs`, `crates/lattice-plugin/src/manifest.rs`, `crates/lattice-cli/src/config.rs`

Use serde + `toml::from_str`, with typed defaults for optional sections. For Phase 1, add `#[serde(deny_unknown_fields)]` to new v1 structs even though no exact current analog exists.

### User-Actionable Errors

**Source:** `crates/lattice-plugin/src/manifest.rs` lines 9-25

Errors should carry the config file path and source/invalid details. Prefer `ProfileError::{Io, Toml, InvalidField, ForbiddenSection, ForbiddenField, PathEscape, MissingDefault}` over `Box<dyn Error>`.

### Path Validation

**Sources:** `manifest.rs::safe_relative_path` lines 296-310, `profile.rs::load_prompt_spec` lines 290-321

Reject absolute paths and `..`; canonicalize target; ensure `prompt.system_path` stays inside the agent package directory, not merely inside cwd.

### Discovery Priority

**Sources:** `registry.rs` lines 38-73, `lattice_dir.rs` lines 108-140

Keep agent packages at `<agents-dir>/<name>/agent.toml`. Load global first, overlay project-local second. Explicit path lookup bypasses named search.

### Tool Vocabulary

**Source:** `crates/lattice-tools/src/capabilities.rs` lines 15-25 and 114-163

Keep bundle names: `core`, `workspace`, `filesystem`, `editing`, `shell`, `planning`, `web`. Delete presets: `minimal`, `reader`, `coding`, `code-agent`.

### Pure Lowering

**Source:** `crates/lattice-orchestrator/src/agent_spec.rs` lines 179-196 and 235-250

Lowering copies already-loaded profile data into pure data specs. It must not construct live `Agent`, call runtime, resolve models, read sessions, resolve credentials, build sandbox, or expand actual tool definitions.

### Default Agent Template

**Source:** `crates/lattice-cli/src/commands/new_agent.rs` lines 5-40 and 42-70

Keep command ergonomics and file-write skeleton, but move v1 template generation into `lattice-profile`. Template must visibly say it is auto-generated and should be edited for project role, prompt, tools, and security settings.

## No Analog Found

| File/Concern | Role | Data Flow | Reason |
|---|---|---|---|
| Strict v1 TOML allowlist with `#[serde(deny_unknown_fields)]` | model/validation | transform | Current repo mostly uses permissive serde defaults. Phase 1 must introduce strict profile parsing and tests. |
| Interactive default-agent creation prompt | controller/service boundary | request-response + file-I/O | Existing `new_agent` writes when explicitly invoked; no current “prompt before auto-create default” pattern. Keep prompting in CLI, pure template/filesystem helpers in `lattice-profile`. |
| `lattice-profile` crate boundary | config/API | transform | New crate must be narrower than old orchestrator; no exact existing crate has this dependency position. |

## Negative Copy List

Do not copy these old profile/runtime concepts into `lattice-profile`:

- `AgentPreset`, `AgentPreset::Coding`, `coding`, `code-agent`, `ToolPreset`, `CapabilitySelection.presets`.
- `[capabilities]`, `[handoff]`, `[orchestrator]`, `[plugins]`, `[pipeline]`, `[subagents]`, `[micro_agents]`, `[memory]`.
- `agent.type`, `agent.preset`, `agent.skippable`, `agent.tags`.
- `behavior_type`, `confidence_threshold`, `max_retries`, `enforce_sandbox` as old orchestrator behavior fields.
- `lattice_runtime`, `lattice_orchestrator`, `lattice_agent`, `lattice_session`, `lattice_plugin`, CLI/TUI imports inside `lattice-profile`.
- Hash sidecar creation from `load_profile_verified` unless explicitly re-scoped; Phase 1 decisions did not require it.

## Metadata

**Analog search scope:** `crates/lattice-orchestrator`, `crates/lattice-tools`, `crates/lattice-runtime`, `crates/lattice-cli`, `crates/lattice-plugin`, root and crate `Cargo.toml` files.
**Files scanned:** required 12 groups plus targeted analogs (`commands/mod.rs`, `config.rs`, `manifest.rs`, `watcher.rs`).
**Pattern extraction date:** 2026-05-14
