//! CEP execution envelope types.
//!
//! Defines the structured envelope that describes intent, capabilities,
//! scope, impact, constraints, and guard verdicts for controlled execution.

use crate::capability::RequiredCapability;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// CEP enforcement mode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CepMode {
    Enforce,
    Warn,
    Off,
}

/// The kind of impact an operation can have.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ImpactKind {
    WorkspaceObservation,
    WorkspaceMutation,
    CommandExecution,
    ExternalObservation,
    SessionStateMutation,
    AuditObservation,
}

/// A const-constructible, order-canonical set of [`ImpactKind`] values.
///
/// Used in [`EffectDecl`] to declare the static effect ceiling for a method.
/// Always declare members in [`ImpactKind`] enum declaration order.
/// Serializes as a JSON array of snake_case strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EffectKindSet(pub &'static [ImpactKind]);

impl EffectKindSet {
    pub const fn from_static(kinds: &'static [ImpactKind]) -> Self {
        Self(kinds)
    }

    pub fn contains(&self, kind: &ImpactKind) -> bool {
        self.0.iter().any(|k| k == kind)
    }

    pub fn is_subset_of(&self, other: &EffectKindSet) -> bool {
        self.0.iter().all(|k| other.contains(k))
    }
}

impl Serialize for EffectKindSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl JsonSchema for EffectKindSet {
    fn schema_name() -> String {
        "EffectKindSet".to_owned()
    }
    fn json_schema(gen: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        <Vec<ImpactKind>>::json_schema(gen)
    }
}

impl From<EffectKindSet> for BTreeSet<ImpactKind> {
    fn from(kinds: EffectKindSet) -> Self {
        kinds.0.iter().cloned().collect()
    }
}

/// Static scope-region kind descriptor for effect declarations.
///
/// Describes the *class* of scope a method operates in, not concrete
/// per-invocation bounds (those are executor-owned).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectScopeKind {
    PathRead,
    ObservationRoot,
    WorkspaceWrite,
    CommandExecution,
    ExternalHost,
    SessionState,
}

/// Static method-level effect declaration (the permission ceiling).
///
/// Catalog methods carry one of these as `const EFFECT`. The `kinds` field is a
/// set (not kind + also).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EffectDecl {
    pub kinds: EffectKindSet,
    pub scope: EffectScopeKind,
    #[serde(serialize_with = "serialize_static_slice")]
    #[schemars(with = "Vec<RequiredCapability>")]
    pub capabilities: &'static [RequiredCapability],
}

fn serialize_static_slice<T: Serialize, S: serde::Serializer>(
    slice: &'static [T],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    slice.serialize(serializer)
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
    FetchUrl { host: String },
    MutateSessionState,
    Inspect,
    ReadAudit,
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
///
/// New fields are additive: old JSON without `scope`, `confidence`, or
/// `unknowns` deserializes cleanly with `#[serde(default)]` on each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Impact {
    pub kind: ImpactKind,
    pub target: String,
    /// Optional structural scope for the impact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ImpactScope>,
    /// How certain the impact assessment is.
    #[serde(default)]
    pub confidence: ImpactConfidence,
    /// Unknown or unclassified side-effects the analyzer could not resolve.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknowns: Vec<ImpactUnknown>,
}

/// Structural scope of an impact.
///
/// Describes what part of the system is affected. Based on a `PathSet`-style
/// model: `ExactFile` and `Directory` are single-path, `FileSet` is a collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImpactScope {
    ExactFile(PathBuf),
    Directory(PathBuf),
    FileSet(Vec<PathBuf>),
    Host(String),
    SessionState,
}

impl Default for ImpactScope {
    /// Unresolved scope defaults to `Directory(PathBuf::new())` — only triggered
    /// by explicit `"scope": {}` in JSON, which means "unresolved scope".
    fn default() -> Self {
        Self::Directory(PathBuf::new())
    }
}

/// Confidence level of an impact assessment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImpactConfidence {
    Complete,
    #[default]
    Conservative,
    Partial,
}

/// An unknown or unclassified impact that the static analyzer could not resolve.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactUnknown {
    pub kind: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// A verification command to run after execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyCommand {
    pub command: String,
    pub expected_exit_code: Option<i32>,
}

/// What to do on verification failure.
///
/// Current mutation semantics retain the diff in the workspace when
/// verification fails. Read-only methods do not retain anything. There is no
/// rollback mechanism in the protocol contract yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnFailure {
    /// Abort the call, surface the error, leave the workspace in its current state.
    Abort,
    /// Retain the failing diff in the workspace and allow follow-up patches
    /// under the same envelope.
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
/// Runtime-injected default envelope every turn starts with.
///
/// It may carry `ReadPath`, `Inspect`, and optionally `MutateSessionState` and
/// `FetchUrl(host)` capabilities. It never grants `WritePath` or `RunCommand`;
/// those capabilities require an explicitly declared envelope.
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
                    scope: None,
                    confidence: ImpactConfidence::Conservative,
                    unknowns: vec![],
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
    /// Session-state mutation is allowed in the base envelope because it is not
    /// a workspace mutation.
    pub fn with_session_state(mut self) -> Self {
        self.inner
            .capabilities
            .insert(Capability::MutateSessionState);
        self
    }

    /// Add a `FetchUrl(host)` capability to this base envelope.
    ///
    /// External observation is allowed in the base envelope when a host is
    /// configured. One call adds one host; call repeatedly to allow more hosts.
    pub fn with_fetch_host(mut self, host: impl Into<String>) -> Self {
        self.inner
            .capabilities
            .insert(Capability::FetchUrl { host: host.into() });
        self
    }

    /// Add session-scoped `ReadAudit` for provenance query/replay.
    pub fn with_read_audit(mut self) -> Self {
        self.inner.capabilities.insert(Capability::ReadAudit);
        self
    }

    /// Verify that this base envelope never grants `WritePath` or `RunCommand`.
    ///
    /// Those two capabilities are reserved for explicitly declared envelopes.
    /// All other capabilities — `ReadPath`, `Inspect`, `MutateSessionState`,
    /// `FetchUrl` — are allowed in a base envelope.
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
                | Capability::MutateSessionState
                | Capability::ReadAudit => {}
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
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
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
    fn base_envelope_accepts_read_audit() {
        let base = BaseExecutionEnvelope::for_workspace_root("/home/user/project", "audit read")
            .with_read_audit();
        assert!(base.validate_no_write_or_command().is_ok());
        assert!(base
            .inner
            .capabilities
            .iter()
            .any(|c| matches!(c, Capability::ReadAudit)));

        let mut inner = base.inner.clone();
        inner.capabilities.insert(Capability::WritePath {
            path_pattern: "/tmp".into(),
        });
        let bad = BaseExecutionEnvelope { inner };
        assert!(bad.validate_no_write_or_command().is_err());
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
        // Rollback is not in the wire contract. Make stale payloads fail loudly.
        let result: Result<OnFailure, _> = serde_json::from_str("\"rollback\"");
        assert!(result.is_err());
    }

    #[test]
    fn effect_kind_set_const_construction() {
        const _: EffectKindSet = EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]);
        let set = EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]);
        assert_eq!(set.0.len(), 1);
    }

    #[test]
    fn effect_kind_set_subset_true() {
        let small = EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]);
        let big = EffectKindSet::from_static(&[
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
        ]);
        assert!(small.is_subset_of(&big));
    }

    #[test]
    fn effect_kind_set_subset_false() {
        let big = EffectKindSet::from_static(&[
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
        ]);
        let small = EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]);
        assert!(!big.is_subset_of(&small));
    }

    #[test]
    fn effect_kind_set_contains() {
        let set = EffectKindSet::from_static(&[ImpactKind::CommandExecution]);
        assert!(set.contains(&ImpactKind::CommandExecution));
        assert!(!set.contains(&ImpactKind::WorkspaceMutation));
    }

    #[test]
    fn effect_kind_set_serializes_as_snake_case_array() {
        let set = EffectKindSet::from_static(&[ImpactKind::WorkspaceMutation]);
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["workspace_mutation"]"#);
    }

    #[test]
    fn effect_kind_set_serializes_composite_as_snake_case_array() {
        let set = EffectKindSet::from_static(&[
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
        ]);
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["workspace_mutation","command_execution"]"#);
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
    fn impact_kind_has_exactly_six_variants() {
        let variants: Vec<String> = [
            ImpactKind::WorkspaceObservation,
            ImpactKind::WorkspaceMutation,
            ImpactKind::CommandExecution,
            ImpactKind::ExternalObservation,
            ImpactKind::SessionStateMutation,
            ImpactKind::AuditObservation,
        ]
        .iter()
        .map(|v| serde_json::to_string(v).unwrap().replace('"', ""))
        .collect();
        assert_eq!(variants.len(), 6);
        assert_eq!(variants[0], "workspace_observation");
        assert_eq!(variants[1], "workspace_mutation");
        assert_eq!(variants[2], "command_execution");
        assert_eq!(variants[3], "external_observation");
        assert_eq!(variants[4], "session_state_mutation");
        assert_eq!(variants[5], "audit_observation");
    }

    #[test]
    fn audit_observation_impact_kind_round_trips() {
        let kind = ImpactKind::AuditObservation;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"audit_observation\"");
        let parsed: ImpactKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    #[test]
    fn read_audit_capability_round_trips() {
        let cap = Capability::ReadAudit;
        let json = serde_json::to_string(&cap).unwrap();
        assert_eq!(json, "\"read_audit\"");
        let parsed: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, cap);
    }

    #[test]
    fn impact_deserializes_old_json_without_new_fields() {
        let old_json = json!({"kind": "workspace_mutation", "target": "src/main.rs"});
        let impact: Impact = serde_json::from_value(old_json).unwrap();
        assert_eq!(impact.kind, ImpactKind::WorkspaceMutation);
        assert_eq!(impact.target, "src/main.rs");
        assert_eq!(impact.scope, None);
        assert_eq!(impact.confidence, ImpactConfidence::Conservative);
        assert!(impact.unknowns.is_empty());
    }

    #[test]
    fn impact_round_trips_with_new_fields() {
        let impact = Impact {
            kind: ImpactKind::WorkspaceMutation,
            target: "src/main.rs".to_string(),
            scope: Some(ImpactScope::ExactFile(PathBuf::from("src/main.rs"))),
            confidence: ImpactConfidence::Complete,
            unknowns: vec![ImpactUnknown {
                kind: "syscall".to_string(),
                description: "unknown system call effect".to_string(),
                location: Some("src/lib.rs:42".to_string()),
            }],
        };
        let serialized = serde_json::to_string(&impact).unwrap();
        let deserialized: Impact = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, impact);
    }

    #[test]
    fn impact_with_partial_unknowns_round_trips() {
        let impact = Impact {
            kind: ImpactKind::CommandExecution,
            target: "gcc".to_string(),
            scope: Some(ImpactScope::Host("build-server".to_string())),
            confidence: ImpactConfidence::Partial,
            unknowns: vec![
                ImpactUnknown {
                    kind: "network".to_string(),
                    description: "may fetch dependencies".to_string(),
                    location: None,
                },
                ImpactUnknown {
                    kind: "filesystem".to_string(),
                    description: "writes object files".to_string(),
                    location: Some("build/".to_string()),
                },
            ],
        };
        let serialized = serde_json::to_string(&impact).unwrap();
        let deserialized: Impact = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, impact);
    }

    #[test]
    fn impact_scope_file_set_round_trips() {
        let scope = ImpactScope::FileSet(vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
        let json = serde_json::to_string(&scope).unwrap();
        let parsed: ImpactScope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, scope);
    }

    #[test]
    fn impact_confidence_default_is_conservative() {
        assert_eq!(ImpactConfidence::default(), ImpactConfidence::Conservative);
    }

    #[test]
    fn impact_scope_default_is_empty_directory() {
        assert_eq!(
            ImpactScope::default(),
            ImpactScope::Directory(PathBuf::new())
        );
    }

    #[test]
    fn impact_unknown_location_is_optional() {
        let without_loc = ImpactUnknown {
            kind: "filesystem".to_string(),
            description: "unknown side effect".to_string(),
            location: None,
        };
        let json = serde_json::to_string(&without_loc).unwrap();
        let parsed: ImpactUnknown = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, without_loc);
        // location should be absent, not null
        assert!(!json.contains("location"));
    }

    #[test]
    fn impact_scope_host_round_trips() {
        let scope = ImpactScope::Host("api.github.com".to_string());
        let json = serde_json::to_string(&scope).unwrap();
        assert!(json.contains("api.github.com"));
        let parsed: ImpactScope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, scope);
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
                host: "example.com".into(),
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
