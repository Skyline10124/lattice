use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use lattice_core::types::{BehaviorMode, YoloSandboxPolicy};
pub use lattice_plugin::orchestration::{DagEdgeConfig, PluginSlotConfig, PluginsConfig};

// ---------------------------------------------------------------------------
// ProfileError — errors raised during profile loading and verification
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("prompt spec error: {0}")]
    PromptSpec(String),

    #[error("hash file corrupted: invalid hex")]
    InvalidHash,

    #[error("profile tampered: {path} (expected {expected}, found {found})")]
    Tampered {
        path: PathBuf,
        expected: String,
        found: String,
    },
}

// ---------------------------------------------------------------------------
// AgentProfile — a TOML-backed micro-agent definition
// ---------------------------------------------------------------------------

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
    /// Computed — resolved from plugins_toml in load().
    #[serde(skip)]
    pub plugins: Option<PluginsConfig>,
    /// Computed — loaded from prompt_path in load().
    #[serde(skip)]
    pub prompt_spec: Option<lattice_prompt::PromptSpec>,
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
    #[deprecated(
        since = "0.7.0",
        note = "Use PromptSpec.content via prompt_path instead."
    )]
    pub prompt: String,
    #[deprecated(
        since = "0.7.0",
        note = "Use PromptSpec.content via prompt_path instead."
    )]
    #[serde(default)]
    pub file: Option<String>, // optional external prompt file
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CapabilitiesConfig {
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub bundles: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolsConfig {
    #[serde(default)]
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
    /// Sandbox policy for YOLO mode: "allowlist", "nobash", or omitted.
    #[serde(default)]
    pub enforce_sandbox: Option<String>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            behavior_type: default_behavior_type(),
            confidence_threshold: default_confidence_threshold(),
            max_retries: default_max_retries(),
            enforce_sandbox: None,
        }
    }
}

fn default_behavior_type() -> String {
    "strict".into()
}
fn default_confidence_threshold() -> f64 {
    0.7
}
fn default_max_retries() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct OrchestratorConfigProfile {
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
    pub fallback: Option<lattice_core::handoff::HandoffTarget>,
    #[serde(default)]
    pub output_schema: Option<String>, // JSON schema for output validation
    #[serde(default)]
    pub max_turns: Option<u32>, // max agent turns in pipeline (default: 10)
}

// ---------------------------------------------------------------------------
// Plugin DAG config (intra-agent orchestration)
// ---------------------------------------------------------------------------

// TOML intermediate — converts BehaviorModeToml → BehaviorMode
// Supports backward-compatible "yolo" / "strict" mode strings with
// optional `enforce_sandbox` field for YOLO mode.
#[derive(Debug, Clone, Deserialize)]
struct BehaviorModeToml {
    mode: String,
    #[serde(default)]
    confidence_threshold: Option<f64>,
    #[serde(default)]
    max_retries: Option<u32>,
    #[serde(default)]
    escalate_to: Option<String>,
    #[serde(default)]
    enforce_sandbox: Option<String>, // "allowlist", "nobash", or absent
}

impl BehaviorModeToml {
    fn to_behavior_mode(&self) -> Option<BehaviorMode> {
        match self.mode.as_str() {
            "yolo" => {
                let policy = match self.enforce_sandbox.as_deref() {
                    Some("nobash") => YoloSandboxPolicy::NoBash,
                    Some("allowlist") | None => YoloSandboxPolicy::EnforceCommandAllowlist,
                    Some(other) => {
                        tracing::warn!(
                            "unknown enforce_sandbox '{}' in yolo mode, using EnforceCommandAllowlist",
                            other
                        );
                        YoloSandboxPolicy::EnforceCommandAllowlist
                    }
                };
                Some(BehaviorMode::Yolo {
                    enforce_sandbox: policy,
                })
            }
            "strict" => Some(BehaviorMode::Strict {
                confidence_threshold: self.confidence_threshold.unwrap_or(0.7),
                max_retries: self.max_retries.unwrap_or(3),
                escalate_to: self.escalate_to.clone(),
            }),
            other => {
                tracing::warn!("unknown behavior mode '{}' in slot, ignoring", other);
                None
            }
        }
    }
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
            behavior: raw.behavior.and_then(|b| b.to_behavior_mode()),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct PluginsConfigToml {
    entry: String,
    #[serde(default)]
    slots: Vec<PluginSlotConfigToml>,
    #[serde(default)]
    edges: Vec<DagEdgeConfig>,
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
    prompt_path: Option<String>,
    #[serde(default)]
    capabilities: CapabilitiesConfig,
    #[serde(default)]
    tools: ToolsConfig,
    #[serde(default)]
    behavior: BehaviorConfig,
    #[serde(default)]
    handoff: HandoffConfig,
    #[serde(default, rename = "plugins")]
    plugins_toml: Option<PluginsConfigToml>,
    #[serde(default)]
    orchestrator: OrchestratorConfigProfile,
    #[serde(default)]
    memory: MemoryConfigProfile,
}

// ---------------------------------------------------------------------------
// PromptSpec loading
// ---------------------------------------------------------------------------

/// Load a PromptSpec from a prompt.toml file.
/// Security: rejects absolute paths, paths containing `..`, and paths resolving outside CWD.
pub(crate) fn load_prompt_spec(
    profile_dir: &Path,
    prompt_path: &str,
) -> Result<lattice_prompt::PromptSpec, Box<dyn std::error::Error>> {
    let path = Path::new(prompt_path);
    if path.is_absolute() || prompt_path.contains("..") {
        return Err(format!(
            "prompt_path '{}' rejected: must be a relative path without '..'",
            prompt_path
        )
        .into());
    }
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
    let content = std::fs::read_to_string(&canonical)
        .map_err(|e| format!("prompt_path '{}' read failed: {e}", prompt_path))?;
    let spec: lattice_prompt::PromptSpec = toml::from_str(&content)?;
    spec.validate()?;
    Ok(spec)
}

// ---------------------------------------------------------------------------
// AgentProfile — loading
// ---------------------------------------------------------------------------

impl AgentProfile {
    /// Load a profile from a TOML file without integrity verification.
    /// Prefer `load_profile_verified()` for production use — it provides
    /// blake3 hash verification against a `.toml.hash` sidecar.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let raw: AgentProfileRaw = toml::from_str(&content)?;
        let mut profile = AgentProfile {
            agent: raw.agent,
            system: raw.system,
            capabilities: raw.capabilities,
            tools: raw.tools,
            behavior: raw.behavior,
            handoff: raw.handoff,
            orchestrator: raw.orchestrator,
            memory: raw.memory,
            plugins: None,
            prompt_spec: None,
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
        if let Some(ref prompt_path) = raw.prompt_path {
            let profile_dir = path.parent().unwrap_or(Path::new("."));
            let spec = load_prompt_spec(profile_dir, prompt_path)?;
            profile.prompt_spec = Some(spec);
        }
        Ok(profile)
    }

    /// Load a profile from a directory (directory/agent.toml).
    pub fn load_from_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load(&dir.join("agent.toml"))
    }

    /// Resolve the effective system prompt (file content or inline).
    ///
    /// When `system.file` is set, the file MUST exist and be readable —
    /// errors are propagated rather than silently falling back to the inline
    /// prompt.  This prevents misconfigured agents from running with a
    /// generic prompt when the operator explicitly requested a file-based one.
    ///
    /// Security: resolves symlinks via canonicalize and verifies the resolved
    /// path is within the current working directory to prevent traversal attacks.
    /// Absolute paths and paths containing `..` are rejected as hard errors.
    #[allow(deprecated)]
    pub fn system_prompt(&self) -> Result<String, Box<dyn std::error::Error>> {
        if let Some(ref file) = self.system.file {
            let path = Path::new(file);
            if path.is_absolute() || file.contains("..") {
                return Err(format!(
                    "system.file '{}' rejected: must be a relative path without '..'",
                    file
                )
                .into());
            }
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| format!("system.file '{}' not found: {e}", file))?;
            if let Ok(cwd) = std::env::current_dir() {
                if !canonical.starts_with(&cwd) {
                    return Err(format!(
                        "system.file '{}' resolved to '{}' which is outside the working directory",
                        file,
                        canonical.to_string_lossy()
                    )
                    .into());
                }
            }
            let content = std::fs::read_to_string(&canonical)
                .map_err(|e| format!("system.file '{}' read failed: {e}", file))?;
            return Ok(content);
        }
        Ok(self.system.prompt.clone())
    }
}

// ---------------------------------------------------------------------------
// Blake3 hash verification — load_profile_verified
// ---------------------------------------------------------------------------

/// Load and verify a profile's integrity via blake3 hash.
///
/// First load: computes the blake3 hash of the TOML content and writes a
/// `.toml.hash` sidecar file (hex-encoded).  Subsequent loads: verifies the
/// current content hash matches the stored sidecar.  If the verification fails
/// a [`ProfileError::Tampered`] is returned.
///
/// Returns `(AgentProfile, blake3_hash_bytes)` on success.
pub fn load_profile_verified(path: &Path) -> Result<(AgentProfile, [u8; 32]), ProfileError> {
    load_profile_verified_inner(path, true)
}

/// Load and verify a profile without creating a missing `.toml.hash` sidecar.
///
/// If a sidecar already exists, it is still enforced. If no sidecar exists,
/// the profile is parsed and returned read-only. This is intended for
/// discovery/validation paths that must not mutate agent directories.
pub fn load_profile_verified_read_only(
    path: &Path,
) -> Result<(AgentProfile, [u8; 32]), ProfileError> {
    load_profile_verified_inner(path, false)
}

fn load_profile_verified_inner(
    path: &Path,
    write_missing_hash: bool,
) -> Result<(AgentProfile, [u8; 32]), ProfileError> {
    let content = std::fs::read_to_string(path)?;
    let hash: [u8; 32] = blake3::hash(content.as_bytes()).into();

    let hash_path = path.with_extension("toml.hash");
    let hash_sidecar_exists = hash_path.exists();

    if hash_sidecar_exists {
        // Verify against stored hash
        let stored_hex = std::fs::read_to_string(&hash_path)?;
        let stored: Vec<u8> =
            hex::decode(stored_hex.trim()).map_err(|_| ProfileError::InvalidHash)?;
        if hash.as_slice() != stored.as_slice() {
            return Err(ProfileError::Tampered {
                path: path.to_owned(),
                expected: hex::encode(hash),
                found: hex::encode(stored),
            });
        }
    }

    let profile: AgentProfile = toml::from_str(&content)?;

    // Load prompt_spec if prompt_path is set
    let mut profile = profile;
    let raw_check: AgentProfileRaw = toml::from_str(&content)?;
    if let Some(ref prompt_path) = raw_check.prompt_path {
        let profile_dir = path.parent().unwrap_or(Path::new("."));
        let spec = load_prompt_spec(profile_dir, prompt_path)
            .map_err(|e| ProfileError::PromptSpec(e.to_string()))?;
        profile.prompt_spec = Some(spec);
    }

    if !hash_sidecar_exists && write_missing_hash {
        // First successful load — write hash file only after schema/prompt
        // validation succeeds. Failed discovery/validation attempts must stay
        // read-only and must not leave `.toml.hash` sidecars behind.
        std::fs::write(&hash_path, hex::encode(hash))?;
    }

    Ok((profile, hash))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(deprecated)]
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
    fn test_capabilities_section_deserializes_presets_and_bundles() {
        let toml_str = r#"
        [agent]
        name = "reader-plus-web"
        model = "sonnet"

        [system]
        prompt = "Read and fetch."

        [capabilities]
        presets = ["reader"]
        bundles = ["web"]
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(profile.capabilities.presets, vec!["reader"]);
        assert_eq!(profile.capabilities.bundles, vec!["web"]);
    }

    #[test]
    fn test_missing_capabilities_deserializes_empty() {
        let toml_str = r#"
        [agent]
        name = "minimal"
        model = "sonnet"

        [system]
        prompt = "Minimal"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert!(profile.capabilities.presets.is_empty());
        assert!(profile.capabilities.bundles.is_empty());
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
        assert_eq!(profile.behavior.behavior_type, "strict");
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
    fn test_orchestrator_and_memory_sections() {
        let toml_str = r#"
        [agent]
        name = "security-reviewer"
        model = "sonnet"

        [system]
        prompt = "You are a security specialist."

        [orchestrator]
        subscribe = ["code-changes", "review-requests"]
        publish = ["security-findings"]
        rpc = ["refactorer"]

        [memory]
        shared_read = ["review-results", "refactor-plans"]
        shared_write = ["security-findings"]
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert_eq!(
            profile.orchestrator.subscribe,
            vec!["code-changes", "review-requests"]
        );
        assert_eq!(profile.orchestrator.publish, vec!["security-findings"]);
        assert_eq!(profile.orchestrator.rpc, vec!["refactorer"]);
        assert_eq!(
            profile.memory.shared_read,
            vec!["review-results", "refactor-plans"]
        );
        assert_eq!(profile.memory.shared_write, vec!["security-findings"]);
    }

    #[test]
    fn test_default_orchestrator_and_memory() {
        let toml_str = r#"
        [agent]
        name = "minimal"
        model = "sonnet"

        [system]
        prompt = "Minimal"
        "#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert!(profile.orchestrator.subscribe.is_empty());
        assert!(profile.orchestrator.publish.is_empty());
        assert!(profile.orchestrator.rpc.is_empty());
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
            Some(lattice_core::handoff::HandoffTarget::Single(
                "fallback-agent".into()
            ))
        );
        assert_eq!(profile.handoff.handoff_rules.len(), 2);
        assert_eq!(
            profile.handoff.handoff_rules[0].target,
            Some(lattice_core::handoff::HandoffTarget::Single(
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
            capabilities: CapabilitiesConfig::default(),
            tools: ToolsConfig::default(),
            behavior: BehaviorConfig::default(),
            handoff: HandoffConfig::default(),
            orchestrator: OrchestratorConfigProfile::default(),
            memory: MemoryConfigProfile::default(),
            plugins: None,
            prompt_spec: None,
        };
        // Absolute path is rejected as a hard error
        assert!(profile.system_prompt().is_err());
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
            capabilities: CapabilitiesConfig::default(),
            tools: ToolsConfig::default(),
            behavior: BehaviorConfig::default(),
            handoff: HandoffConfig::default(),
            orchestrator: OrchestratorConfigProfile::default(),
            memory: MemoryConfigProfile::default(),
            plugins: None,
            prompt_spec: None,
        };
        // Path containing ".." is rejected as a hard error
        assert!(profile.system_prompt().is_err());
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

    // verify_config_integrity + sha2 removed — superseded by load_profile_verified (blake3)

    // -----------------------------------------------------------------------
    // Blake3 hash verification tests
    // -----------------------------------------------------------------------

    /// First load: no .toml.hash sidecar exists, so one is auto-generated and
    /// the profile is loaded successfully.
    #[test]
    fn test_blake3_first_load_creates_hash() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("test_agent.toml");
        let content =
            "[agent]\nname = \"test\"\nmodel = \"sonnet\"\n\n[system]\nprompt = \"test\"\n";
        std::fs::write(&toml_path, content).unwrap();

        let result = load_profile_verified(&toml_path);
        assert!(
            result.is_ok(),
            "First load should succeed: {:?}",
            result.err()
        );

        // Sidecar should have been created
        let hash_path = toml_path.with_extension("toml.hash");
        assert!(
            hash_path.exists(),
            ".toml.hash sidecar should exist after first load"
        );

        // Verify the hash file contains valid hex
        let stored = std::fs::read_to_string(&hash_path).unwrap();
        assert!(
            hex::decode(stored.trim()).is_ok(),
            "Stored hash should be valid hex"
        );
    }

    /// Second load with unchanged content: hash matches, load succeeds.
    #[test]
    fn test_blake3_second_load_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("test_agent.toml");
        let content =
            "[agent]\nname = \"test\"\nmodel = \"sonnet\"\n\n[system]\nprompt = \"test\"\n";
        std::fs::write(&toml_path, content).unwrap();

        // First load — creates hash
        let (profile1, hash1) = load_profile_verified(&toml_path).unwrap();
        assert_eq!(profile1.agent.name, "test");

        // Second load — verifies hash
        let (profile2, hash2) = load_profile_verified(&toml_path).unwrap();
        assert_eq!(profile2.agent.name, "test");
        assert_eq!(hash1, hash2, "Hash should be consistent across loads");
    }

    /// Tampered profile: content changed but hash not updated, load must fail.
    #[test]
    fn test_blake3_tampered_profile_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("test_agent.toml");
        let content =
            "[agent]\nname = \"test\"\nmodel = \"sonnet\"\n\n[system]\nprompt = \"test\"\n";
        std::fs::write(&toml_path, content).unwrap();

        // First load — creates hash
        let _ = load_profile_verified(&toml_path).unwrap();

        // Tamper: modify content WITHOUT updating the hash
        let tampered_content =
            "[agent]\nname = \"evil\"\nmodel = \"sonnet\"\n\n[system]\nprompt = \"hacked\"\n";
        std::fs::write(&toml_path, tampered_content).unwrap();

        let result = load_profile_verified(&toml_path);
        assert!(
            matches!(result, Err(ProfileError::Tampered { .. })),
            "Tampered profile should be rejected, got: {:?}",
            result
        );
    }

    /// Corrupted hash file: invalid hex, load must fail with InvalidHash.
    #[test]
    fn test_blake3_corrupted_hash_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("test_agent.toml");
        let content =
            "[agent]\nname = \"test\"\nmodel = \"sonnet\"\n\n[system]\nprompt = \"test\"\n";
        std::fs::write(&toml_path, content).unwrap();

        // First load — creates hash
        let _ = load_profile_verified(&toml_path).unwrap();

        // Corrupt the hash file with non-hex content
        let hash_path = toml_path.with_extension("toml.hash");
        std::fs::write(&hash_path, "not valid hex!!!").unwrap();

        let result = load_profile_verified(&toml_path);
        assert!(
            matches!(result, Err(ProfileError::InvalidHash)),
            "Corrupted hash should be rejected, got: {:?}",
            result
        );
    }

    // -----------------------------------------------------------------------
    // PromptSpec loading tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_prompt_path_rejects_absolute_path() {
        let result = load_prompt_spec(Path::new("."), "/etc/passwd");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be a relative path"));
    }

    #[test]
    fn test_prompt_path_rejects_path_traversal() {
        let result = load_prompt_spec(Path::new("."), "../secret.toml");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be a relative path"));
    }

    #[test]
    fn test_prompt_path_loads_valid_spec() {
        let cwd = std::env::current_dir().unwrap();
        let dir = cwd.join(format!("test-prompt-spec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt_toml = dir.join("prompt.toml");
        std::fs::write(
            &prompt_toml,
            r#"
version = 1
content = "You are a {{role}}."

[variables]
role = "reviewer"
"#,
        )
        .unwrap();

        let spec = load_prompt_spec(&dir, "prompt.toml").unwrap();
        assert_eq!(spec.version, 1);
        assert_eq!(spec.content, Some("You are a {{role}}.".to_string()));
        assert_eq!(spec.variables.get("role").unwrap(), "reviewer");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_no_prompt_path_means_no_spec() {
        let toml_str = r#"
[agent]
name = "test"
model = "sonnet"

[system]
prompt = "test prompt"
"#;
        let profile: AgentProfile = toml::from_str(toml_str).unwrap();
        assert!(profile.prompt_spec.is_none());
    }

    #[test]
    fn test_load_with_prompt_path() {
        let cwd = std::env::current_dir().unwrap();
        let dir = cwd.join(format!("test-prompt-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prompt_toml = dir.join("prompt.toml");
        std::fs::write(
            &prompt_toml,
            r#"
version = 1
content = "Custom prompt"
"#,
        )
        .unwrap();

        let agent_toml = dir.join("agent.toml");
        std::fs::write(
            &agent_toml,
            r#"
prompt_path = "prompt.toml"

[agent]
name = "test"
model = "sonnet"

[system]
prompt = "old prompt"
"#,
        )
        .unwrap();

        let profile = AgentProfile::load(&agent_toml).unwrap();
        assert!(profile.prompt_spec.is_some());
        let spec = profile.prompt_spec.unwrap();
        assert_eq!(spec.content, Some("Custom prompt".to_string()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
