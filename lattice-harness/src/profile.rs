use serde::{Deserialize, Serialize};
use std::path::Path;

use lattice_plugin::bundle::BehaviorMode;

// ---------------------------------------------------------------------------
// AgentProfile — a TOML-backed micro-agent definition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentProfile {
    pub agent: AgentConfig,
    pub system: SystemConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub behavior: BehaviorConfig,
    #[serde(default)]
    pub handoff: HandoffConfig,
    #[serde(default)]
    pub bus: BusConfigProfile,
    #[serde(default)]
    pub memory: MemoryConfigProfile,
    /// Computed — resolved from plugins_toml in load().
    #[serde(skip)]
    pub plugins: Option<PluginsConfig>,
}

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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemConfig {
    pub prompt: String,
    #[serde(default)]
    pub file: Option<String>, // optional external prompt file
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolsConfig {
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BehaviorConfig {
    #[serde(default = "default_behavior_type")]
    pub behavior_type: String, // "strict" | "yolo"
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            behavior_type: default_behavior_type(),
            confidence_threshold: default_confidence_threshold(),
            max_retries: default_max_retries(),
        }
    }
}

fn default_behavior_type() -> String {
    "yolo".into()
}
fn default_confidence_threshold() -> f64 {
    0.7
}
fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct BusConfigProfile {
    #[serde(default)]
    pub subscribe: Vec<String>,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub rpc: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct MemoryConfigProfile {
    #[serde(default)]
    pub shared_read: Vec<String>,
    #[serde(default)]
    pub shared_write: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HandoffConfig {
    #[serde(default, rename = "rules")]
    pub handoff_rules: Vec<crate::handoff_rule::HandoffRule>,
    #[serde(default)]
    pub fallback: Option<crate::handoff_rule::HandoffTarget>,
    #[serde(default)]
    pub output_schema: Option<String>, // JSON schema for output validation
    #[serde(default)]
    pub max_turns: Option<u32>, // max agent turns in pipeline (default: 10)
}

// ---------------------------------------------------------------------------
// Plugin DAG config (intra-agent orchestration)
// ---------------------------------------------------------------------------

/// Optional plugin-based agent execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginsConfig {
    pub entry: String,
    #[serde(default)]
    pub slots: Vec<PluginSlotConfig>,
    #[serde(default)]
    pub edges: Vec<AgentEdgeConfig>,
    #[serde(default)]
    pub shared_tools: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSlotConfig {
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub model_override: Option<String>,
    pub max_turns: Option<u32>,
    pub behavior: Option<BehaviorMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEdgeConfig {
    pub from: String,
    pub rule: crate::handoff_rule::HandoffRule,
}

// TOML intermediate — converts BehaviorModeToml → BehaviorMode
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorModeToml {
    mode: String,
    #[serde(default)]
    confidence_threshold: Option<f64>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    escalate_to: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct PluginSlotConfigToml {
    name: String,
    plugin: String,
    #[serde(default)]
    tools: Vec<String>,
    model_override: Option<String>,
    max_turns: Option<u32>,
    behavior: Option<BehaviorModeToml>,
}

impl From<PluginSlotConfigToml> for PluginSlotConfig {
    fn from(raw: PluginSlotConfigToml) -> Self {
        Self {
            name: raw.name,
            plugin: raw.plugin,
            tools: raw.tools,
            model_override: raw.model_override,
            max_turns: raw.max_turns,
            behavior: raw.behavior.and_then(|b| match b.mode.as_str() {
                "yolo" => Some(BehaviorMode::Yolo),
                "strict" => Some(BehaviorMode::Strict {
                    confidence_threshold: b.confidence_threshold.unwrap_or(0.7),
                    max_retries: b.max_retries.unwrap_or(3),
                    escalate_to: b.escalate_to,
                }),
                other => {
                    tracing::warn!("unknown behavior mode '{}' in slot, ignoring", other);
                    None
                }
            }),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PluginsConfigToml {
    entry: String,
    #[serde(default)]
    slots: Vec<PluginSlotConfigToml>,
    #[serde(default)]
    edges: Vec<AgentEdgeConfig>,
    #[serde(default)]
    shared_tools: Vec<String>,
}

impl From<PluginsConfigToml> for PluginsConfig {
    fn from(raw: PluginsConfigToml) -> Self {
        Self {
            entry: raw.entry,
            slots: raw.slots.into_iter().map(Into::into).collect(),
            edges: raw.edges,
            shared_tools: raw.shared_tools,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct AgentProfileRaw {
    agent: AgentConfig,
    system: SystemConfig,
    #[serde(default)]
    tools: ToolsConfig,
    #[serde(default)]
    behavior: BehaviorConfig,
    #[serde(default)]
    handoff: HandoffConfig,
    #[serde(default, rename = "plugins")]
    plugins_toml: Option<PluginsConfigToml>,
    #[serde(default)]
    bus: BusConfigProfile,
    #[serde(default)]
    memory: MemoryConfigProfile,
}

// ---------------------------------------------------------------------------
// AgentProfile — loading
// ---------------------------------------------------------------------------

impl AgentProfile {
    /// Load a profile from a TOML file.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let raw: AgentProfileRaw = toml::from_str(&content)?;
        let mut profile = AgentProfile {
            agent: raw.agent,
            system: raw.system,
            tools: raw.tools,
            behavior: raw.behavior,
            handoff: raw.handoff,
            bus: raw.bus,
            memory: raw.memory,
            plugins: None,
        };
        if let Some(plugins_toml) = raw.plugins_toml {
            let config: PluginsConfig = plugins_toml.into();
            if !config.slots.iter().any(|s| s.name == config.entry) {
                return Err(
                    format!("entry slot '{}' not found in [plugins.slots]", config.entry).into(),
                );
            }
            profile.plugins = Some(config);
        }
        Ok(profile)
    }

    /// Load a profile from a directory (directory/agent.toml).
    pub fn load_from_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&dir.join("agent.toml"))
    }

    /// Resolve the effective system prompt (file content or inline).
    /// Only allows relative paths — absolute paths and paths containing `..` are rejected.
    pub fn system_prompt(&self) -> String {
        if let Some(ref file) = self.system.file {
            let path = Path::new(file);
            // Reject absolute paths and path traversal
            if path.is_absolute() || file.contains("..") {
                tracing::warn!(
                    "system.file '{}' rejected: must be a relative path without '..'",
                    file
                );
                return self.system.prompt.clone();
            }
            if path.exists() {
                return std::fs::read_to_string(path)
                    .unwrap_or_else(|_| self.system.prompt.clone());
            }
        }
        self.system.prompt.clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_profile_from_toml() {
        let toml_str = r#"
        [agent]
        name = "code-review"
        model = "deepseek-v4-pro"
        skippable = true
        tags = ["review", "code"]

        [system]
        prompt = "You are a code reviewer."

        [tools]
        enabled = ["read_file", "grep"]

        [behavior]
        behavior_type = "strict"
        confidence_threshold = 0.8
        max_retries = 2

        [handoff]
        fallback = "refactor"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(profile.agent.name, "code-review");
        assert_eq!(profile.agent.model, "deepseek-v4-pro");
        assert!(profile.agent.skippable);
        assert_eq!(profile.agent.tags, vec!["review", "code"]);
        assert_eq!(profile.tools.enabled, vec!["read_file", "grep"]);
        assert_eq!(profile.behavior.behavior_type, "strict");
    }

    #[test]
    fn test_default_behavior() {
        let toml_str = r#"
        [agent]
        name = "test"
        model = "test-model"

        [system]
        prompt = "Test prompt"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(profile.behavior.behavior_type, "yolo");
        assert_eq!(profile.behavior.max_retries, 3);
    }

    #[test]
    fn test_default_skippable_and_tags() {
        let toml_str = r#"
        [agent]
        name = "default-test"
        model = "test-model"

        [system]
        prompt = "Test prompt"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert!(!profile.agent.skippable);
        assert!(profile.agent.tags.is_empty());
    }

    #[test]
    fn test_bus_and_memory_sections() {
        let toml_str = r#"
        [agent]
        name = "security-reviewer"
        model = "sonnet"

        [system]
        prompt = "You are a security specialist."

        [bus]
        subscribe = ["code-changes", "review-requests"]
        publish = ["security-findings"]
        rpc = ["refactorer"]

        [memory]
        shared_read = ["review-results", "refactor-plans"]
        shared_write = ["security-findings"]
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(
            profile.bus.subscribe,
            vec!["code-changes", "review-requests"]
        );
        assert_eq!(profile.bus.publish, vec!["security-findings"]);
        assert_eq!(profile.bus.rpc, vec!["refactorer"]);
        assert_eq!(
            profile.memory.shared_read,
            vec!["review-results", "refactor-plans"]
        );
        assert_eq!(profile.memory.shared_write, vec!["security-findings"]);
    }

    #[test]
    fn test_default_bus_and_memory() {
        let toml_str = r#"
        [agent]
        name = "minimal"
        model = "sonnet"

        [system]
        prompt = "Minimal"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert!(profile.bus.subscribe.is_empty());
        assert!(profile.bus.publish.is_empty());
        assert!(profile.bus.rpc.is_empty());
        assert!(profile.memory.shared_read.is_empty());
        assert!(profile.memory.shared_write.is_empty());
    }

    #[test]
    fn test_handoff_rules_deserialization() {
        let toml_str = r#"
        [agent]
        name = "test-agent"
        model = "sonnet"

        [system]
        prompt = "Test"

        [handoff]
        fallback = "fallback-agent"

        [[handoff.rules]]
        condition = { field = "confidence", op = "<", value = "0.5" }
        target = "human-review"

        [[handoff.rules]]
        default = true
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(profile.agent.name, "test-agent");
        assert_eq!(
            profile.handoff.fallback,
            Some(crate::handoff_rule::HandoffTarget::Single(
                "fallback-agent".into()
            ))
        );
        assert_eq!(profile.handoff.handoff_rules.len(), 2);
        assert_eq!(
            profile.handoff.handoff_rules[0].target,
            Some(crate::handoff_rule::HandoffTarget::Single(
                "human-review".into()
            ))
        );
        assert!(profile.handoff.handoff_rules[1].default);
    }

    #[test]
    fn test_system_prompt_rejects_absolute_path() {
        let profile = AgentProfile {
            agent: AgentConfig {
                name: "test".into(),
                model: "sonnet".into(),
                description: String::new(),
                skippable: false,
                tags: vec![],
            },
            system: SystemConfig {
                prompt: "inline prompt".into(),
                file: Some("/etc/passwd".into()),
            },
            tools: ToolsConfig::default(),
            behavior: BehaviorConfig::default(),
            handoff: HandoffConfig::default(),
            bus: BusConfigProfile::default(),
            memory: MemoryConfigProfile::default(),
            plugins: None,
        };
        // Absolute path is rejected, falls back to inline prompt
        assert_eq!(profile.system_prompt(), "inline prompt");
    }

    #[test]
    fn test_system_prompt_rejects_path_traversal() {
        let profile = AgentProfile {
            agent: AgentConfig {
                name: "test".into(),
                model: "sonnet".into(),
                description: String::new(),
                skippable: false,
                tags: vec![],
            },
            system: SystemConfig {
                prompt: "inline prompt".into(),
                file: Some("../secret.txt".into()),
            },
            tools: ToolsConfig::default(),
            behavior: BehaviorConfig::default(),
            handoff: HandoffConfig::default(),
            bus: BusConfigProfile::default(),
            memory: MemoryConfigProfile::default(),
            plugins: None,
        };
        // Path containing ".." is rejected, falls back to inline prompt
        assert_eq!(profile.system_prompt(), "inline prompt");
    }

    #[test]
    fn test_plugins_config_toml_deserialize() {
        let toml_str = r#"
entry = "review"

[[slots]]
name = "review"
plugin = "CodeReview"
max_turns = 3
behavior = { mode = "strict", confidence_threshold = 0.8, max_retries = 2 }

[[slots]]
name = "refactor"
plugin = "Refactor"

[[edges]]
from = "review"
rule = { condition = { field = "confidence", op = ">", value = "0.5" }, target = "refactor" }

[[edges]]
from = "refactor"
rule = { default = true }
"#;
        let config: PluginsConfigToml = toml::from_str(toml_str).unwrap();
        let config: PluginsConfig = config.into();
        assert_eq!(config.entry, "review");
        assert_eq!(config.slots.len(), 2);
        assert!(matches!(
            config.slots[0].behavior,
            Some(BehaviorMode::Strict { .. })
        ));
        assert_eq!(config.edges.len(), 2);
    }
}
