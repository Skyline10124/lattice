use artemis_agent::Agent;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub handoff_file: Option<String>, // path to handoff.py
    #[serde(default)]
    pub fallback: Option<String>, // default next agent if handoff returns None
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
                return std::fs::read_to_string(path).unwrap_or_else(|_| self.system.prompt.clone());
            }
        }
        self.system.prompt.clone()
    }
}

// ---------------------------------------------------------------------------
// AgentRegistry — loads and indexes agent profiles from a directory
// ---------------------------------------------------------------------------

pub struct AgentRegistry {
    agents: std::collections::HashMap<String, AgentProfile>,
    base_dir: PathBuf,
}

impl AgentRegistry {
    /// Load all TOML agent profiles from ~/.artemis/agents/ or a custom path.
    pub fn load_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut registry = Self {
            agents: std::collections::HashMap::new(),
            base_dir: dir.to_path_buf(),
        };

        if !dir.exists() {
            return Ok(registry);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let agent_toml = entry.path().join("agent.toml");
                if agent_toml.exists() {
                    match AgentProfile::load(&agent_toml) {
                        Ok(profile) => {
                            let name = profile.agent.name.clone();
                            registry.agents.insert(name, profile);
                        }
                        Err(e) => {
                            eprintln!(
                                "Warning: failed to load agent at {}: {}",
                                agent_toml.display(),
                                e
                            );
                        }
                    }
                }
            }
        }

        Ok(registry)
    }

    pub fn get(&self, name: &str) -> Option<&AgentProfile> {
        self.agents.get(name)
    }

    pub fn list(&self) -> Vec<&AgentProfile> {
        self.agents.values().collect()
    }
}

// ---------------------------------------------------------------------------
// PythonHandoff — executes handoff.py scripts via PyO3
// ---------------------------------------------------------------------------

/// Execute a Python handoff function and return the next agent name.
pub fn run_python_handoff(
    script_path: &Path,
    output: &serde_json::Value,
    confidence: f64,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use pyo3::types::PyAnyMethods;
    let script = std::fs::read_to_string(script_path)?;
    let output_json = serde_json::to_string(output)?;
    let code_cstr =
        std::ffi::CString::new(script).map_err(|e| format!("script contains null byte: {e}"))?;

    pyo3::Python::attach(|py| {
        let module = pyo3::types::PyModule::from_code(
            py,
            &code_cstr,
            c"handoff.py",
            c"handoff",
        )?;

        let result = module.call_method1("should_handoff", (&output_json[..], confidence))?;

        if result.is_none() {
            Ok(None)
        } else {
            Ok(Some(result.extract::<String>()?))
        }
    })
}

// ---------------------------------------------------------------------------
// AgentRunner — wires AgentProfile + Agent + Python handoff
// ---------------------------------------------------------------------------

/// A runner that uses an AgentProfile to create and run an Agent.
pub struct AgentRunner {
    pub profile: AgentProfile,
    pub agent: Agent,
    pub handoff_script: Option<String>,
}

impl AgentRunner {
    /// Create a runner from a profile, resolving the model and loading tools/handoff.
    pub fn from_profile(profile: AgentProfile, agent: Agent) -> Self {
        let handoff_script = profile.handoff.handoff_file.as_ref().and_then(|f| {
            std::fs::read_to_string(f).ok()
        });

        Self {
            profile,
            agent,
            handoff_script,
        }
    }

    /// Run the agent with the given input. Returns the output and optional next agent.
    pub fn run(
        &mut self,
        input: &str,
    ) -> Result<(serde_json::Value, Option<String>), Box<dyn std::error::Error>> {
        let events = self.agent.run(input, 10);
        // Extract text content from events
        let mut content = String::new();
        for event in &events {
            if let artemis_agent::LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }

        // Parse as JSON
        let output: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({"content": content}));

        // Run handoff if configured
        let next = if let Some(ref script) = self.handoff_script {
            // Write script to temp file so run_python_handoff can read it
            let tmp = std::env::temp_dir().join(format!("artemis_handoff_{}.py", self.profile.agent.name));
            std::fs::write(&tmp, script)?;
            let result = run_python_handoff(&tmp, &output, 0.8)?; // default confidence
            let _ = std::fs::remove_file(&tmp);
            result
        } else {
            self.profile.handoff.fallback.clone()
        };

        Ok((output, next))
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
}
