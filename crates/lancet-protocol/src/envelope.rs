//! CEP execution envelope types.
//!
//! Defines the structured envelope that describes intent, capabilities,
//! scope, impact, constraints, and guard verdicts for controlled execution.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// CEP enforcement mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CepMode {
    Enforce,
    Warn,
    Off,
}

/// The kind of impact an operation can have.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ImpactKind {
    WorkspaceObservation,
    WorkspaceMutation,
    CommandExecution,
    ExternalObservation,
    SessionStateMutation,
}

/// A closed enumeration of capabilities an envelope can grant.
#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum Capability {
    ReadPath { path_pattern: String },
    WritePath { path_pattern: String },
    RunCommand { argv_allowlist: Vec<String> },
    FetchUrl { host_pattern: String },
    MutateSessionState,
    Inspect,
}

/// Guard verdict on an execution envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GuardVerdict {
    Allowed,
    Denied { reason: String },
    HumanReview { reason: String },
}

/// The intent of an execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Intent {
    pub description: String,
}

/// The scope of an execution.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default, flatten)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// Constraints on execution behavior.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Constraints {
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_mutations: Option<u32>,
    #[serde(default, flatten)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// An impact declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Impact {
    pub kind: ImpactKind,
    pub target: String,
}

/// A verification command to run after execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyCommand {
    pub command: String,
    pub expected_exit_code: Option<i32>,
}

/// What to do on verification failure.
///
/// Alpha semantics (per CEP-04 / MUT-03): verification failure for
/// `WorkspaceMutation` retains the diff in the workspace; read/observation
/// methods do not retain anything. There is **no** rollback mechanism in
/// v1.3-alpha — shadow workspace transactional verification is deferred to
/// v1.3-beta+.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Abort the call, surface the error, leave the workspace in its current state.
    Abort,
    /// Retain the failing diff in the workspace; let the agent submit follow-up
    /// patches under the same envelope (the alpha mutation default per MUT-03).
    RetainDiff,
    /// Continue execution despite the failure (use sparingly; intended for
    /// best-effort observation chains).
    Continue,
}

/// The full CEP execution envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionEnvelope {
    pub intent: Intent,
    pub capabilities: BTreeSet<Capability>,
    pub scope: Scope,
    pub impact: Vec<Impact>,
    pub constraints: Constraints,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<VerifyCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_failure: Option<OnFailure>,
    #[serde(default, flatten)]
    pub extensions: serde_json::Map<String, serde_json::Value>,
}

/// A base execution envelope restricted to non-mutation, non-command capabilities.
///
/// Per CEP-08, `BaseExecutionEnvelope` is the runtime-injected default envelope
/// every turn starts with. It may carry `ReadPath`, `Inspect`, and optionally
/// `MutateSessionState` and `FetchUrl(host_pattern)` capabilities; it **never**
/// grants `WritePath` or `RunCommand`. Those two capabilities require an
/// explicitly declared envelope (Phase 22 `declare_envelope` / `submit_intent`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BaseExecutionEnvelope {
    pub inner: ExecutionEnvelope,
}

impl BaseExecutionEnvelope {
    /// Create a base envelope for a workspace root with read-only capabilities.
    pub fn for_workspace_root(root: &str, intent_description: &str) -> Self {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::ReadPath {
            path_pattern: root.to_string(),
        });
        capabilities.insert(Capability::Inspect);

        Self {
            inner: ExecutionEnvelope {
                intent: Intent {
                    description: intent_description.to_string(),
                },
                capabilities,
                scope: Scope {
                    workspace_root: Some(root.to_string()),
                    extensions: serde_json::Map::new(),
                },
                impact: vec![Impact {
                    kind: ImpactKind::WorkspaceObservation,
                    target: root.to_string(),
                }],
                constraints: Constraints::default(),
                verify: None,
                on_failure: None,
                extensions: serde_json::Map::new(),
            },
        }
    }

    /// Add a `MutateSessionState` capability to this base envelope.
    ///
    /// Per CEP-08, session-state mutation is allowed in the base envelope
    /// (it is not a workspace mutation). Phase 21 (PROF-02) consumes this
    /// builder when wiring `session.todo_write`.
    pub fn with_session_state(mut self) -> Self {
        self.inner
            .capabilities
            .insert(Capability::MutateSessionState);
        self
    }

    /// Add a `FetchUrl(host_pattern)` capability to this base envelope.
    ///
    /// Per CEP-08, external observation is allowed in the base envelope when
    /// a host pattern is configured (PROF-02 `[cep.network]`). One call adds
    /// one host pattern; call repeatedly to allow more hosts.
    pub fn with_fetch_host(mut self, host_pattern: impl Into<String>) -> Self {
        self.inner.capabilities.insert(Capability::FetchUrl {
            host_pattern: host_pattern.into(),
        });
        self
    }

    /// Verify that this base envelope never grants `WritePath` or `RunCommand`.
    ///
    /// Per CEP-08, those two capabilities are reserved for explicitly declared
    /// envelopes (Phase 22). All other capabilities — `ReadPath`, `Inspect`,
    /// `MutateSessionState`, `FetchUrl` — are allowed in a base envelope.
    pub fn validate_no_write_or_command(&self) -> Result<(), &'static str> {
        for cap in &self.inner.capabilities {
            match cap {
                Capability::WritePath { .. } => {
                    return Err("BaseExecutionEnvelope cannot contain WritePath");
                }
                Capability::RunCommand { .. } => {
                    return Err("BaseExecutionEnvelope cannot contain RunCommand");
                }
                Capability::ReadPath { .. }
                | Capability::Inspect
                | Capability::FetchUrl { .. }
                | Capability::MutateSessionState => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_impact_kind_fails_deserialization() {
        let bad = json!("unknown_impact");
        let result: Result<ImpactKind, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_capability_fails_deserialization() {
        let bad = json!({"file_read": {"path_pattern": "/tmp"}});
        let result: Result<Capability, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    #[test]
    fn execution_envelope_round_trips_with_empty_extensions() {
        let envelope = ExecutionEnvelope {
            intent: Intent {
                description: "read workspace".into(),
            },
            capabilities: BTreeSet::from_iter([
                Capability::ReadPath {
                    path_pattern: "/tmp".into(),
                },
                Capability::Inspect,
            ]),
            scope: Scope {
                workspace_root: Some("/tmp".into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceObservation,
                target: "/tmp".into(),
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };
        let serialized = serde_json::to_string(&envelope).unwrap();
        let deserialized: ExecutionEnvelope = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, envelope);
    }

    #[test]
    fn execution_envelope_preserves_unknown_extension_fields() {
        let json = json!({
            "intent": {"description": "test"},
            "capabilities": [{"inspect": null}],
            "scope": {"workspace_root": "/tmp", "custom_scope_field": true},
            "impact": [{"kind": "workspace_observation", "target": "/tmp"}],
            "constraints": {"timeout_ms": 5000, "custom_constraint": "extra"},
            "verify": null,
            "on_failure": null,
            "custom_extension": 42
        });
        let envelope: ExecutionEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(envelope.extensions["custom_extension"], 42);
        assert_eq!(envelope.scope.extensions["custom_scope_field"], true);
        assert_eq!(
            envelope.constraints.extensions["custom_constraint"],
            "extra"
        );

        // Round-trip preserves extensions
        let serialized = serde_json::to_string(&envelope).unwrap();
        let deserialized: ExecutionEnvelope = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.extensions["custom_extension"], 42);
    }

    #[test]
    fn base_envelope_for_workspace_root_includes_read_and_inspect() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "inspect repo");
        let caps = &base.inner.capabilities;
        let has_read = caps
            .iter()
            .any(|c| matches!(c, Capability::ReadPath { .. }));
        let has_inspect = caps.iter().any(|c| matches!(c, Capability::Inspect));
        assert!(has_read);
        assert!(has_inspect);
    }

    #[test]
    fn base_envelope_excludes_write_path() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "inspect repo");
        assert!(base.validate_no_write_or_command().is_ok());

        let mut inner = base.inner.clone();
        inner.capabilities.insert(Capability::WritePath {
            path_pattern: "/tmp".into(),
        });
        let bad_base = BaseExecutionEnvelope { inner };
        assert!(bad_base.validate_no_write_or_command().is_err());
    }

    #[test]
    fn base_envelope_excludes_run_command() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "inspect repo");
        assert!(base.validate_no_write_or_command().is_ok());

        let mut inner = base.inner.clone();
        inner.capabilities.insert(Capability::RunCommand {
            argv_allowlist: vec!["ls".into()],
        });
        let bad_base = BaseExecutionEnvelope { inner };
        assert!(bad_base.validate_no_write_or_command().is_err());
    }

    #[test]
    fn base_envelope_accepts_session_state() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "with todos")
            .with_session_state();
        assert!(base.validate_no_write_or_command().is_ok());
        assert!(base
            .inner
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::MutateSessionState)));
    }

    #[test]
    fn base_envelope_accepts_fetch_host() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "with fetch")
            .with_fetch_host("example.com")
            .with_fetch_host("api.github.com");
        assert!(base.validate_no_write_or_command().is_ok());
        let host_count = base
            .inner
            .capabilities
            .iter()
            .filter(|c| matches!(c, Capability::FetchUrl { .. }))
            .count();
        assert_eq!(host_count, 2);
    }

    #[test]
    fn on_failure_round_trips_retain_diff() {
        let mode = OnFailure::RetainDiff;
        let serialized = serde_json::to_string(&mode).unwrap();
        assert_eq!(serialized, "\"retain_diff\"");
        let parsed: OnFailure = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, OnFailure::RetainDiff);
    }

    #[test]
    fn on_failure_rollback_is_rejected() {
        // Rollback was removed: shadow workspace transactional verification
        // is deferred to v1.3-beta+. Make sure stale wire payloads fail loudly.
        let result: Result<OnFailure, _> = serde_json::from_str("\"rollback\"");
        assert!(result.is_err());
    }

    #[test]
    fn tool_call_params_are_direct_envelope_fields() {
        let envelope = ExecutionEnvelope {
            intent: Intent {
                description: "read file".into(),
            },
            capabilities: BTreeSet::from_iter([Capability::ReadPath {
                path_pattern: "/tmp".into(),
            }]),
            scope: Scope {
                workspace_root: Some("/tmp".into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };

        let call: crate::frame::ToolCall<ExecutionEnvelope> =
            crate::jsonrpc::Request::new(crate::jsonrpc::Id::Number(1), "execute".into(), envelope);
        let serialized = serde_json::to_string(&call).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        // params is the direct envelope, not nested under a CEP wrapper
        assert!(parsed["params"]["intent"]["description"] == "read file");
        assert!(parsed["params"].get("envelope").is_none());
    }

    #[test]
    fn impact_kind_has_exactly_five_variants() {
        let variants: Vec<String> = [
            ImpactKind::WorkspaceObservation,
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
            ImpactKind::ExternalObservation,
            ImpactKind::SessionStateMutation,
        ]
        .iter()
        .map(|v| serde_json::to_string(v).unwrap().replace('"', ""))
        .collect();
        assert_eq!(variants.len(), 5);
        assert_eq!(variants[0], "workspace_observation");
        assert_eq!(variants[1], "workspace_mutation");
        assert_eq!(variants[2], "command_execution");
        assert_eq!(variants[3], "external_observation");
        assert_eq!(variants[4], "session_state_mutation");
    }

    #[test]
    fn capability_no_old_variants() {
        let current: Vec<Capability> = vec![
            Capability::ReadPath {
                path_pattern: "/tmp".into(),
            },
            Capability::WritePath {
                path_pattern: "/tmp".into(),
            },
            Capability::RunCommand {
                argv_allowlist: vec!["ls".into()],
            },
            Capability::FetchUrl {
                host_pattern: "example.com".into(),
            },
            Capability::MutateSessionState,
            Capability::Inspect,
        ];
        for cap in &current {
            let s = serde_json::to_string(cap).unwrap();
            assert!(!s.contains("file_read"));
            assert!(!s.contains("file_write"));
            assert!(!s.contains("\"shell\""));
            assert!(!s.contains("\"network\""));
            assert!(!s.contains("\"memory\""));
            assert!(!s.contains("\"tool_call\""));
        }
    }
}
