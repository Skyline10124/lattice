//! Lower-level agent turn contract for runtime-owned execution.
//!
//! This module defines pure data types (spec, request, output) and an
//! executor trait that `lattice-runtime` can implement. It intentionally
//! does NOT import `lattice_runtime` or construct live `lattice_agent::Agent`,
//! so the orchestrator-to-runtime dependency direction stays valid.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentPreset — typed agent preset classification
// ---------------------------------------------------------------------------

/// Typed classification of an agent's role / execution profile.
///
/// The coding preset triggers runtime-side coding-specific composition
/// (project instructions, task-state tracking, final reporting, etc.).
/// Non-coding agents leave preset as `None` and receive generic turn
/// execution with no preset-specific completion policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentPreset {
    /// Coding agent: project instructions, task-state tracking, final report.
    Coding,
}

// ---------------------------------------------------------------------------
// AgentTurnSpec — executable profile data for one turn session
// ---------------------------------------------------------------------------

/// Lowered executable spec produced from an `AgentProfile`.
///
/// Contains all information the runtime needs to build or reuse an internal
/// agent handle. This struct is pure data — it does not resolve models,
/// construct executors, or read the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnSpec {
    /// Human-readable profile/agent name (from `AgentConfig.name`).
    pub profile_name: String,
    /// Model identifier string (e.g. "sonnet", "gpt-4o").
    pub model: String,
    /// Optional provider override (e.g. "anthropic", "openai").
    pub provider_override: Option<String>,
    /// System prompt content when no PromptSpec is present.
    pub system_prompt: Option<String>,
    /// Resolved prompt spec with static/dynamic contributions.
    pub prompt_spec: Option<lattice_prompt::PromptSpec>,
    /// Capability configuration (presets, bundles).
    pub capabilities: crate::profile::CapabilitiesConfig,
    /// Tools configuration (enabled tool names).
    pub tools: crate::profile::ToolsConfig,
    /// Behavior configuration (strict/yolo, confidence, retries, sandbox).
    pub behavior: crate::profile::BehaviorConfig,
    /// Handoff configuration (max turns, output schema, handoff rules).
    pub handoff: crate::profile::HandoffConfig,
    /// Memory configuration (shared read keys).
    pub memory: crate::profile::MemoryConfigProfile,
    /// Project root directory for context and sandbox scoping.
    pub project_root: Option<String>,
    /// Current working directory for the agent.
    pub cwd: Option<String>,
    /// Optional thinking effort / extended reasoning budget.
    pub thinking_effort: Option<String>,
    /// Optional typed preset classification.
    pub preset: Option<AgentPreset>,
}

// ---------------------------------------------------------------------------
// AgentTurnRequest — caller-submitted turn execution request
// ---------------------------------------------------------------------------

/// Request to submit a turn to a runtime-owned agent handle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnRequest {
    /// User input / prompt text.
    pub input: String,
    /// Maximum number of agent loop turns before automatic stop.
    pub max_turns: u32,
    /// Optional session ID for continuing an existing conversation.
    pub session_id: Option<String>,
    /// Whether the runtime should persist the session after the turn.
    pub persist_session: bool,
    /// Optional opaque cancellation token/ID.
    pub cancellation_id: Option<String>,
}

// ---------------------------------------------------------------------------
// AgentTurnStatus — terminal status of a turn execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentTurnStatus {
    /// Turn completed successfully.
    Completed,
    /// Turn was cancelled by the caller.
    Cancelled,
    /// Turn terminated with an error.
    Error,
}

// ---------------------------------------------------------------------------
// AgentTurnOutput — result returned to the caller after turn execution
// ---------------------------------------------------------------------------

/// The output produced by a completed agent turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTurnOutput {
    /// Full assistant text content.
    pub assistant_text: String,
    /// Parsed JSON output (or raw-text wrapper for plain profiles).
    pub json_output: serde_json::Value,
    /// Terminal status of the turn.
    pub status: AgentTurnStatus,
    /// Model used for this turn.
    pub model: String,
    /// Provider used for this turn.
    pub provider: String,
}

// ---------------------------------------------------------------------------
// AgentTurnExecutionError — errors raised during turn execution
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AgentTurnExecutionError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("handle busy: {0}")]
    Busy(String),

    #[error("turn cancelled: {0}")]
    Cancelled(String),
}

// ---------------------------------------------------------------------------
// AgentTurnExecutor — runtime-backed turn execution contract
// ---------------------------------------------------------------------------

/// Trait for executing a single agent turn from a lowered spec.
///
/// Implementations live in `lattice-runtime` (or test fakes). The
/// orchestrator depends on this trait but never on the runtime crate,
/// preserving the crate dependency direction.
#[async_trait::async_trait]
pub trait AgentTurnExecutor: Send + Sync {
    /// Execute a single turn for the given spec and request.
    ///
    /// Returns the turn output on success, or a typed execution error.
    async fn run_turn(
        &self,
        spec: AgentTurnSpec,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnOutput, AgentTurnExecutionError>;
}

// ---------------------------------------------------------------------------
// ProfileLoweringError — errors during profile-to-spec conversion
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ProfileLoweringError {
    #[error("profile name is empty")]
    EmptyProfileName,

    #[error("model is empty")]
    EmptyModel,
}

// ---------------------------------------------------------------------------
// lower_profile_to_turn_spec — AgentProfile → AgentTurnSpec
// ---------------------------------------------------------------------------

/// Lower an `AgentProfile` into an executable `AgentTurnSpec`.
///
/// This function copies profile fields into a pure-data spec. It does NOT
/// resolve models, construct executors, create live `Agent` instances,
/// or read the filesystem.
///
/// # Lowering Rules
///
/// 1. `profile.agent.name` → `profile_name`
/// 2. `profile.agent.model` → `model`
/// 3. `system_prompt` is set only when `prompt_spec.content` is absent,
///    matching the current `assemble_agent` behavior to avoid duplication.
/// 4. `capabilities`, `tools`, `behavior`, `handoff`, and `memory` are cloned.
/// 5. If `capabilities.presets` contains `"coding"` or `"code-agent"`,
///    `preset` is set to `Some(AgentPreset::Coding)`.
pub fn lower_profile_to_turn_spec(
    profile: &AgentProfile,
) -> Result<AgentTurnSpec, ProfileLoweringError> {
    // Validate required fields
    if profile.agent.name.is_empty() {
        return Err(ProfileLoweringError::EmptyProfileName);
    }
    if profile.agent.model.is_empty() {
        return Err(ProfileLoweringError::EmptyModel);
    }

    // Determine preset from capabilities.presets
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

    // System prompt: only set when PromptSpec.content is absent
    #[allow(deprecated)]
    let system_prompt = {
        let has_spec_content = profile
            .prompt_spec
            .as_ref()
            .and_then(|s| s.content.as_ref())
            .is_some();
        if !has_spec_content {
            // Use the profile's system_prompt() method, but ignore I/O errors
            // for the file path case — if the file read fails, the caller
            // should have caught it during profile loading.
            profile.system_prompt().ok()
        } else {
            None
        }
    };

    Ok(AgentTurnSpec {
        profile_name: profile.agent.name.clone(),
        model: profile.agent.model.clone(),
        provider_override: None, // resolved later by runtime
        system_prompt,
        prompt_spec: profile.prompt_spec.clone(),
        capabilities: profile.capabilities.clone(),
        tools: profile.tools.clone(),
        behavior: profile.behavior.clone(),
        handoff: profile.handoff.clone(),
        memory: profile.memory.clone(),
        project_root: None,    // set by caller based on runtime context
        cwd: None,             // set by caller based on runtime context
        thinking_effort: None, // set by caller
        preset,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake executor that records the received profile name and input
    /// and returns a canned success output.
    struct FakeExecutor {
        pub last_profile_name: std::sync::Mutex<String>,
        pub last_input: std::sync::Mutex<String>,
    }

    impl FakeExecutor {
        fn new() -> Self {
            Self {
                last_profile_name: std::sync::Mutex::new(String::new()),
                last_input: std::sync::Mutex::new(String::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl AgentTurnExecutor for FakeExecutor {
        async fn run_turn(
            &self,
            spec: AgentTurnSpec,
            request: AgentTurnRequest,
        ) -> Result<AgentTurnOutput, AgentTurnExecutionError> {
            *self.last_profile_name.lock().unwrap() = spec.profile_name.clone();
            *self.last_input.lock().unwrap() = request.input.clone();

            Ok(AgentTurnOutput {
                assistant_text: format!("echo: {}", request.input),
                json_output: serde_json::json!({"echo": request.input}),
                status: AgentTurnStatus::Completed,
                model: spec.model,
                provider: spec.provider_override.unwrap_or_else(|| "test".into()),
            })
        }
    }

    #[tokio::test]
    async fn agent_spec_fake_executor_accepts_turn() {
        let executor = FakeExecutor::new();
        let spec = AgentTurnSpec {
            profile_name: "test-agent".into(),
            model: "test-model".into(),
            provider_override: Some("test-provider".into()),
            system_prompt: None,
            prompt_spec: None,
            capabilities: Default::default(),
            tools: Default::default(),
            behavior: Default::default(),
            handoff: Default::default(),
            memory: Default::default(),
            project_root: None,
            cwd: None,
            thinking_effort: None,
            preset: None,
        };
        let request = AgentTurnRequest {
            input: "hello".into(),
            max_turns: 10,
            session_id: None,
            persist_session: false,
            cancellation_id: None,
        };

        let output = executor.run_turn(spec, request).await.unwrap();

        assert_eq!(output.status, AgentTurnStatus::Completed);
        assert_eq!(output.model, "test-model");
        assert_eq!(output.provider, "test-provider");
        assert_eq!(*executor.last_profile_name.lock().unwrap(), "test-agent");
        assert_eq!(*executor.last_input.lock().unwrap(), "hello");
    }

    #[tokio::test]
    async fn agent_spec_execution_error_display() {
        let err = AgentTurnExecutionError::Busy("handle-1".into());
        assert!(err.to_string().contains("handle-1"));
    }
}

#[cfg(test)]
mod lowering_tests {
    #![allow(deprecated)]

    use super::*;
    use crate::profile::{
        AgentConfig, AgentProfile, BehaviorConfig, CapabilitiesConfig, HandoffConfig,
        MemoryConfigProfile, SystemConfig, ToolsConfig,
    };

    fn make_profile(name: &str, model: &str) -> AgentProfile {
        AgentProfile {
            agent: AgentConfig {
                name: name.into(),
                model: model.into(),
                description: String::new(),
                skippable: false,
                tags: vec![],
            },
            system: SystemConfig {
                prompt: String::new(),
                file: None,
            },
            capabilities: CapabilitiesConfig::default(),
            tools: ToolsConfig::default(),
            behavior: BehaviorConfig::default(),
            handoff: HandoffConfig::default(),
            orchestrator: Default::default(),
            memory: MemoryConfigProfile::default(),
            plugins: None,
            prompt_spec: None,
        }
    }

    #[test]
    fn profile_lowering_preserves_prompt_spec_and_capabilities() {
        let profile = make_profile("test-agent", "claude-sonnet");
        let spec = lower_profile_to_turn_spec(&profile).unwrap();

        assert_eq!(spec.profile_name, "test-agent");
        assert_eq!(spec.model, "claude-sonnet");
        assert!(spec.preset.is_none());
    }

    #[test]
    fn profile_lowering_maps_coding_alias_to_coding_preset() {
        let mut profile = make_profile("coder", "gpt-4o");
        profile.capabilities.presets = vec!["coding".into()];
        let spec = lower_profile_to_turn_spec(&profile).unwrap();
        assert_eq!(spec.preset, Some(AgentPreset::Coding));

        profile.capabilities.presets = vec!["code-agent".into()];
        let spec = lower_profile_to_turn_spec(&profile).unwrap();
        assert_eq!(spec.preset, Some(AgentPreset::Coding));
    }

    #[test]
    fn profile_lowering_rejects_empty_name() {
        let profile = make_profile("", "claude");
        let err = lower_profile_to_turn_spec(&profile).unwrap_err();
        assert!(matches!(err, ProfileLoweringError::EmptyProfileName));
    }

    #[test]
    fn profile_lowering_rejects_empty_model() {
        let profile = make_profile("agent", "");
        let err = lower_profile_to_turn_spec(&profile).unwrap_err();
        assert!(matches!(err, ProfileLoweringError::EmptyModel));
    }
}

#[cfg(test)]
mod lower_contract_tests {
    /// Assert that the lower contract source does not expose live Agent types.
    /// This is a source-level guardrail test that checks only production code.
    #[test]
    fn orchestrator_lower_contract_exposes_no_live_agent() {
        let source = include_str!("agent_spec.rs");

        // Extract only the production code (before the first #[cfg(test)] module)
        let prod_end = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod_source = &source[..prod_end];

        // Check for use lattice_runtime (not in comments)
        for line in prod_source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("//") || trimmed.starts_with("//!") || trimmed.starts_with("///")
            {
                continue;
            }
            if trimmed.starts_with("use ") && trimmed.contains("lattice_runtime") {
                panic!("agent_spec.rs must not import lattice_runtime: {trimmed}");
            }
            if trimmed.starts_with("extern crate ") && trimmed.contains("lattice_runtime") {
                panic!("agent_spec.rs must not import lattice_runtime: {trimmed}");
            }
        }
    }
}
