use serde::{Deserialize, Serialize};
use std::path::Path;

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
pub struct HandoffConfig {
    #[serde(default)]
    pub handoff_file: Option<String>, // path to handoff.py (DEPRECATED — use rules)
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
// AgentProfile — loading
// ---------------------------------------------------------------------------

impl AgentProfile {
    /// Load a profile from a TOML file.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let profile: Self = toml::from_str(&content)?;
        Ok(profile)
    }

    /// Load a profile from a directory (directory/agent.toml).
    pub fn load_from_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&dir.join("agent.toml"))
    }

    /// Resolve the effective system prompt (file content or inline).
    pub fn system_prompt(&self) -> String {
        if let Some(ref file) = self.system.file {
            let path = Path::new(file);
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
}
