//! Runtime-owned agent handle registry and lifecycle.
//!
//! This module keeps live `Agent` instances inside the runtime so callers
//! (CLI, TUI, orchestrator pipeline) never hold mutable agent ownership.
//! Handles are keyed by session/workspace/profile/model/security policy
//! and support reuse, rebuild, dispose, and idle eviction.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use lattice_agent::Agent;
use lattice_core::types::ToolDefinition;
use lattice_core::ResolvedModel;

use crate::RuntimeSecurity;

// ---------------------------------------------------------------------------
// AgentHandleId — opaque handle identifier
// ---------------------------------------------------------------------------

/// Opaque identifier for a runtime-owned agent handle.
///
/// Created by the runtime when a new handle is allocated. Callers pass this
/// id to `submit_turn`, `turn_completion`, `dispose_agent_handle`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentHandleId(String);

impl AgentHandleId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().simple().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AgentHandleId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for AgentHandleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// AgentHandleKey — lookup key for handle reuse
// ---------------------------------------------------------------------------

/// Key used to look up an existing handle for reuse.
///
/// Two requests with equivalent keys share the same handle. The key
/// includes security policy so agents with different sandbox modes
/// never share state.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AgentHandleKey {
    pub session_id: Option<String>,
    pub workspace: PathBuf,
    pub profile_name: Option<String>,
    pub model: String,
    pub provider_override: Option<String>,
    pub preset: Option<lattice_orchestrator::AgentPreset>,
    pub security_policy: String,
}

impl AgentHandleKey {
    /// Build a key. Normalizes `workspace` by canonicalizing when possible.
    pub fn new(workspace: PathBuf, model: String, security_policy: String) -> Self {
        let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
        Self {
            session_id: None,
            workspace,
            profile_name: None,
            model,
            provider_override: None,
            preset: None,
            security_policy,
        }
    }

    pub fn with_session_id(mut self, id: Option<String>) -> Self {
        self.session_id = id;
        self
    }

    pub fn with_profile_name(mut self, name: Option<String>) -> Self {
        self.profile_name = name;
        self
    }

    pub fn with_provider_override(mut self, provider: Option<String>) -> Self {
        self.provider_override = provider;
        self
    }

    pub fn with_preset(mut self, preset: Option<lattice_orchestrator::AgentPreset>) -> Self {
        self.preset = preset;
        self
    }
}

// ---------------------------------------------------------------------------
// AgentHandleSnapshot — public read-only handle metadata
// ---------------------------------------------------------------------------

/// Public snapshot of handle state for callers to inspect without
/// accessing the internal live `Agent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHandleSnapshot {
    pub id: AgentHandleId,
    pub revision: u64,
    pub model: String,
    pub provider: String,
    pub message_count: usize,
    pub task_state_empty: bool,
    pub active_turn: bool,
    pub recent_completion_count: usize,
    pub plugin_overlay_name: Option<String>,
    pub approved_rule_count: usize,
}

/// Terminal status of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    Completed,
    Cancelled,
    Error,
}

/// Runtime-owned plugin context applied to a handle by frontend adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimePluginOverlay {
    pub name: String,
    pub system_prompt_delta: String,
    pub output_contract_delta: Option<String>,
    pub tools: Vec<ToolDefinition>,
    pub authorized_tool_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// HandleState — internal live handle data (not exposed publicly)
// ---------------------------------------------------------------------------

pub(crate) struct HandleState {
    pub id: AgentHandleId,
    pub key: AgentHandleKey,
    pub spec: lattice_orchestrator::AgentTurnSpec,
    pub revision: u64,
    /// Internal live agent. Stored as `Option` so the turn executor can
    /// `take()` it for async execution and replace it afterward.
    pub agent: Option<Agent>,
    pub model: ResolvedModel,
    pub context_length: u32,
    pub tool_definitions: Vec<ToolDefinition>,
    pub security: RuntimeSecurity,
    pub active_turn: bool,
    pub active_turn_id: Option<crate::turn::TurnId>,
    pub active_task_abort: Option<tokio::task::AbortHandle>,
    pub active_assistant_text: String,
    pub cancellation_requested: bool,
    pub plugin_overlay: Option<RuntimePluginOverlay>,
    pub recent_completions: VecDeque<crate::turn::TurnCompletion>,
    pub last_used_at: Instant,
}

// ---------------------------------------------------------------------------
// AgentHandleRegistry — internal registry (Mutex-protected)
// ---------------------------------------------------------------------------

pub(crate) struct AgentHandleRegistry {
    pub by_id: HashMap<AgentHandleId, HandleState>,
    pub by_key: HashMap<AgentHandleKey, AgentHandleId>,
    pub recent_completion_limit: usize,
    #[allow(dead_code)] // consumed by future idle eviction policy
    pub idle_timeout: Duration,
}

impl AgentHandleRegistry {
    pub fn new(recent_completion_limit: usize, idle_timeout: Duration) -> Self {
        Self {
            by_id: HashMap::new(),
            by_key: HashMap::new(),
            recent_completion_limit,
            idle_timeout,
        }
    }
}

impl Default for AgentHandleRegistry {
    fn default() -> Self {
        Self::new(8, Duration::from_secs(600)) // 8 completions, 10 min idle
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_handle_id_is_unique() {
        let a = AgentHandleId::new();
        let b = AgentHandleId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn agent_handle_id_display_and_as_str() {
        let id = AgentHandleId::new();
        assert!(!id.as_str().is_empty());
        assert_eq!(id.to_string(), id.as_str().to_string());
    }

    #[test]
    fn agent_handle_key_normalizes_workspace() {
        let key = AgentHandleKey::new(PathBuf::from("."), "test-model".into(), "project".into());
        assert!(key.workspace.is_absolute());
    }

    #[test]
    fn agent_handle_key_different_policies_produce_different_keys() {
        let k1 = AgentHandleKey::new(PathBuf::from("."), "m".into(), "project".into());
        let k2 = AgentHandleKey::new(PathBuf::from("."), "m".into(), "permissive".into());
        assert_ne!(k1, k2);
    }

    #[test]
    fn agent_handle_registry_default() {
        let reg = AgentHandleRegistry::default();
        assert_eq!(reg.recent_completion_limit, 8);
        assert!(reg.by_id.is_empty());
        assert!(reg.by_key.is_empty());
    }

    #[test]
    fn agent_handle_snapshot_serde_roundtrip() {
        let snap = AgentHandleSnapshot {
            id: AgentHandleId::new(),
            revision: 1,
            model: "test".into(),
            provider: "test".into(),
            message_count: 5,
            task_state_empty: true,
            active_turn: false,
            recent_completion_count: 0,
            plugin_overlay_name: None,
            approved_rule_count: 0,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: AgentHandleSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.revision, 1);
        assert_eq!(back.message_count, 5);
    }

    #[test]
    fn handle_registry_insert_and_lookup() {
        let mut reg = AgentHandleRegistry::default();
        let id = AgentHandleId::new();
        let key = AgentHandleKey::new(PathBuf::from("/tmp"), "m".into(), "p".into());

        // Not inserted yet
        assert!(reg.by_id.get(&id).is_none());

        // Insert placeholder (we can't easily create a real Agent here)
        reg.by_key.insert(key.clone(), id.clone());
        assert!(reg.by_key.contains_key(&key));
    }
}
