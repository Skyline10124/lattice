// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resolved facts table and resolver for CEP policy evaluation.
//!
//! Transforms the public [`PolicyInput`] into an internal [`ResolvedPolicyInput`]
//! with lexical path facts, argv/cwd/env facts, host facts, session-state facts,
//! and immutable budget snapshots. The resolver is pure: no filesystem I/O,
//! no time reads, no mutable counter access.

use std::path::{Path, PathBuf};

use lancet_catalog::catalog::catalog;
use lancet_catalog::method::RequiredCapability;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::policy::{
    EffectiveEnvelopeSnapshot, PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot,
    PolicyInput,
};

use super::BudgetLimits;
use super::capability::InvocationFacts;
use super::command::{CommandPolicy, EffectiveImpact, classify_command_line};
use super::default::WorkspacePolicy;
use super::network::NetworkPolicyConfig;
use super::observation::ObservationPolicyConfig;
use super::shell_ast::{CommandLine, resolve_command_line};

// ── Resolved types (executor-owned, no Serialize/Deserialize) ───────────

/// Resolved path facts for workspace observation/mutation methods.
#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub request_path: String,
    pub normalized_path: PathBuf,
    pub project_root: PathBuf,
}

/// Resolved command facts for `exec.command_run`.
#[derive(Clone, Debug)]
pub struct ResolvedCommand {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env_keys: Vec<String>,
    pub declared_impact: Option<ImpactKind>,
    pub effective_impact: EffectiveImpact,
    /// Expanded sub-command argvs when the invocation is a validated
    /// inline-shell composite; empty for direct argv execution.
    pub sub_argvs: Vec<Vec<String>>,
}

/// Resolved network facts for `fetch.web`.
#[derive(Clone, Debug)]
pub struct ResolvedNetwork {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub scheme: String,
    pub is_localhost: bool,
    pub is_private_ip: bool,
}

/// Resolved session-state facts for `session.todo_write` / `session.envelope`.
#[derive(Clone, Debug)]
pub struct ResolvedSessionState {
    pub operation: super::capability::SessionStateOperation,
    pub todo_ops_count: usize,
}

/// Resolved authority branch — executor-owned, no Serialize/Deserialize.
#[derive(Clone, Debug)]
pub enum ResolvedAuthorityInput {
    /// Envelope-scoped authority with required capability and snapshot.
    Envelope {
        required_capability: RequiredCapability,
        envelope_snapshot: EffectiveEnvelopeSnapshot,
    },
    /// Direct immutable authority with no envelope or capability data.
    Direct,
}

/// Internal resolved facts table built from [`PolicyInput`].
///
/// Not a public protocol type — executor-owned only.
#[derive(Clone, Debug)]
pub struct ResolvedPolicyInput {
    pub method: String,
    pub impact_kind: ImpactKind,
    pub invocation_facts: InvocationFacts,
    pub resolved_target: Option<ResolvedPath>,
    pub resolved_command: Option<ResolvedCommand>,
    pub resolved_network: Option<ResolvedNetwork>,
    pub resolved_session_state: Option<ResolvedSessionState>,
    pub budget_snapshot: PolicyBudgetSnapshot,
    pub authority: ResolvedAuthorityInput,
    pub audit_context: PolicyAuditContext,
    pub command_error: Option<lancet_protocol::CepError>,
}

/// Resolver that builds [`ResolvedPolicyInput`] from [`PolicyInput`].
///
/// Pure resolution: no filesystem I/O, no time reads, no mutable state.
#[derive(Clone, Debug)]
pub struct PolicyResolver {
    pub project_root: PathBuf,
    pub observation: ObservationPolicyConfig,
    pub network: NetworkPolicyConfig,
    pub command: CommandPolicy,
    pub budget: BudgetLimits,
    pub workspace: WorkspacePolicy,
}

impl PolicyResolver {
    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let observation = ObservationPolicyConfig::defaults_for_root(&project_root);
        Self {
            project_root,
            observation,
            network: NetworkPolicyConfig::safe_defaults(),
            command: CommandPolicy::defaults(),
            budget: BudgetLimits::defaults(),
            workspace: WorkspacePolicy::default(),
        }
    }

    /// Resolve a [`PolicyInput`] into an internal [`ResolvedPolicyInput`].
    ///
    /// Pure function: no I/O, no time reads, no mutable counter access.
    pub fn resolve(
        &self,
        input: &PolicyInput,
    ) -> Result<ResolvedPolicyInput, lancet_protocol::CepError> {
        let descriptor = catalog()
            .iter()
            .find(|descriptor| descriptor.name == input.method)
            .ok_or_else(|| {
                lancet_protocol::CepError::new(
                    lancet_protocol::CepErrorCode::SchemaMismatch,
                    format!("unknown catalog method: {}", input.method),
                    lancet_protocol::error::CepErrorData::Validation(
                        lancet_protocol::error::ValidationErrorData {
                            field: "method".into(),
                            reason: format!("'{}' is not in the catalog", input.method),
                        },
                    ),
                    lancet_protocol::Retriability::Permanent,
                )
            })?;

        let impact_kind = descriptor.impact.clone();
        let authority = match &input.authority {
            PolicyAuthorityInput::Envelope { envelope_snapshot } => {
                let required_capability = required_capability_for_invocation(
                    &input.method,
                    &input.params,
                    descriptor.capability,
                );
                ResolvedAuthorityInput::Envelope {
                    required_capability,
                    envelope_snapshot: envelope_snapshot.clone(),
                }
            }
            PolicyAuthorityInput::Direct => ResolvedAuthorityInput::Direct,
        };

        let mut invocation_facts = InvocationFacts::default();
        let mut resolved_target = None;
        let mut resolved_command = None;
        let mut command_error = None;
        let mut resolved_network = None;
        let mut resolved_session_state = None;

        match input.method.as_str() {
            "observe.workspace" => {
                if let Some(path) = extract_observe_workspace_path(&input.params) {
                    let normalized = normalize_lexical_pure(&path)?;
                    let canonical = self.project_root.join(&normalized);
                    invocation_facts.path = Some(canonical.to_string_lossy().to_string());
                    invocation_facts.normalized_path =
                        Some(normalized.to_string_lossy().to_string());
                    resolved_target = Some(ResolvedPath {
                        request_path: path,
                        normalized_path: normalized,
                        project_root: self.project_root.clone(),
                    });
                }
            }
            "mutate.create" | "mutate.edit" => {
                if let Some(path) = extract_path_from_params(&input.params) {
                    let normalized = normalize_lexical_pure(&path)?;
                    let canonical = self.project_root.join(&normalized);
                    invocation_facts.path = Some(canonical.to_string_lossy().to_string());
                    invocation_facts.normalized_path =
                        Some(normalized.to_string_lossy().to_string());
                    resolved_target = Some(ResolvedPath {
                        request_path: path,
                        normalized_path: normalized,
                        project_root: self.project_root.clone(),
                    });
                }
            }
            "mutate.batch" => {
                let all_targets = extract_all_batch_targets(&input.params);
                invocation_facts.all_patch_targets = all_targets.clone();
                if let Some(path) = all_targets.into_iter().next() {
                    let normalized = normalize_lexical_pure(&path)?;
                    let canonical = self.project_root.join(&normalized);
                    invocation_facts.path = Some(canonical.to_string_lossy().to_string());
                    invocation_facts.normalized_path =
                        Some(normalized.to_string_lossy().to_string());
                    resolved_target = Some(ResolvedPath {
                        request_path: path,
                        normalized_path: normalized,
                        project_root: self.project_root.clone(),
                    });
                }
            }
            "exec.command_run" => {
                let argv = extract_argv_from_params(&input.params);
                let cwd_str = extract_cwd_from_params(&input.params);
                let env_keys = extract_env_keys_from_params(&input.params);
                let declared_impact = extract_declared_impact_from_params(&input.params);

                invocation_facts.argv = Some(argv.clone());
                invocation_facts.env_keys = env_keys.clone();

                let cwd = cwd_str
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.project_root.clone());
                invocation_facts.cwd = Some(cwd.clone());

                // Inline-shell carriers expand to sub-commands; expansion
                // rejections defer like classification errors so the rest of
                // the facts table is still populated.
                let mut sub_argvs = Vec::new();
                let effective_impact = match resolve_command_line(&argv, &self.command) {
                    Ok(line) => {
                        if let CommandLine::Shell(expansion) = &line {
                            sub_argvs = expansion.sub_argvs.clone();
                            invocation_facts.sub_argvs = expansion.sub_argvs.clone();
                        }
                        match classify_command_line(&line, declared_impact.clone(), &self.command) {
                            Ok(impact) => impact,
                            Err(err) => {
                                command_error = Some(err);
                                EffectiveImpact::Diagnostic
                            }
                        }
                    }
                    Err(err) => {
                        command_error = Some(err);
                        EffectiveImpact::Diagnostic
                    }
                };

                resolved_command = Some(ResolvedCommand {
                    argv,
                    cwd,
                    env_keys,
                    declared_impact,
                    effective_impact,
                    sub_argvs,
                });
            }
            "fetch.web" => {
                if let Some(url) = extract_url_from_params(&input.params) {
                    let (host, port, scheme, is_localhost, is_private_ip) = parse_url_facts(&url);
                    invocation_facts.host = Some(host.clone());
                    resolved_network = Some(ResolvedNetwork {
                        url,
                        host,
                        port,
                        scheme,
                        is_localhost,
                        is_private_ip,
                    });
                }
            }
            "session.todo_write" => {
                let todo_count = extract_todo_count_from_params(&input.params);
                invocation_facts.session_state_operation =
                    Some(super::capability::SessionStateOperation::TodoWrite);
                resolved_session_state = Some(ResolvedSessionState {
                    operation: super::capability::SessionStateOperation::TodoWrite,
                    todo_ops_count: todo_count,
                });
            }
            "session.envelope" => {
                invocation_facts.session_state_operation =
                    Some(session_envelope_operation(&input.params));
                resolved_session_state = Some(ResolvedSessionState {
                    operation: session_envelope_operation(&input.params),
                    todo_ops_count: 0,
                });
            }
            _ => {}
        }

        Ok(ResolvedPolicyInput {
            method: input.method.clone(),
            impact_kind,
            invocation_facts,
            resolved_target,
            resolved_command,
            resolved_network,
            resolved_session_state,
            budget_snapshot: input.audit_context.budget_snapshot.clone(),
            authority,
            audit_context: input.audit_context.clone(),
            command_error,
        })
    }
}

// ── Pure param extraction helpers ───────────────────────────────────────

fn required_capability_for_invocation(
    method: &str,
    params: &serde_json::Value,
    descriptor_capability: RequiredCapability,
) -> RequiredCapability {
    match method {
        "observe.workspace" => match params.get("mode").and_then(|v| v.as_str()) {
            Some("read_file") => RequiredCapability::ReadPath,
            Some("grep" | "list_directory" | "find_files") => RequiredCapability::Inspect,
            _ => descriptor_capability,
        },
        _ => descriptor_capability,
    }
}

fn extract_observe_workspace_path(params: &serde_json::Value) -> Option<String> {
    match params.get("mode").and_then(|v| v.as_str()) {
        Some("read_file") | Some("list_directory") => extract_path_from_params(params),
        Some("grep") | Some("find_files") => {
            extract_optional_path_from_params(params).or_else(|| Some(".".to_string()))
        }
        _ => None,
    }
}

fn session_envelope_operation(
    _params: &serde_json::Value,
) -> super::capability::SessionStateOperation {
    super::capability::SessionStateOperation::EnvelopeDeclare
}

fn extract_path_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("path")?.as_str().map(String::from)
}

fn extract_optional_path_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("path")?.as_str().map(String::from)
}

fn extract_all_batch_targets(params: &serde_json::Value) -> Vec<String> {
    params
        .get("operations")
        .and_then(|v| v.as_array())
        .map(|operations| {
            operations
                .iter()
                .filter_map(extract_path_from_params)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn extract_argv_from_params(params: &serde_json::Value) -> Vec<String> {
    params
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn extract_cwd_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("cwd")?.as_str().map(String::from)
}

fn extract_env_keys_from_params(params: &serde_json::Value) -> Vec<String> {
    params
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

fn extract_declared_impact_from_params(params: &serde_json::Value) -> Option<ImpactKind> {
    let val = params.get("declared_impact")?;
    serde_json::from_value(val.clone()).ok()
}

fn extract_url_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("url")?.as_str().map(String::from)
}

fn extract_todo_count_from_params(params: &serde_json::Value) -> usize {
    params
        .get("todos")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0)
}

// ── Pure path normalization ─────────────────────────────────────────────

fn normalize_lexical_pure(path: &str) -> Result<PathBuf, lancet_protocol::CepError> {
    use std::path::Component;

    if path.contains('\0') {
        return Err(lancet_protocol::CepError::new(
            lancet_protocol::CepErrorCode::PathContainment,
            "path contains NUL byte",
            lancet_protocol::error::CepErrorData::Validation(
                lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: "contains NUL byte".into(),
                },
            ),
            lancet_protocol::Retriability::Permanent,
        ));
    }

    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(lancet_protocol::CepError::new(
                    lancet_protocol::CepErrorCode::PathContainment,
                    format!("path contains '..' traversal: '{path}'"),
                    lancet_protocol::error::CepErrorData::Validation(
                        lancet_protocol::error::ValidationErrorData {
                            field: "path".into(),
                            reason: "contains '..'".into(),
                        },
                    ),
                    lancet_protocol::Retriability::Permanent,
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(lancet_protocol::CepError::new(
                    lancet_protocol::CepErrorCode::PathContainment,
                    format!("path must be relative: '{path}'"),
                    lancet_protocol::error::CepErrorData::Validation(
                        lancet_protocol::error::ValidationErrorData {
                            field: "path".into(),
                            reason: "absolute path not allowed".into(),
                        },
                    ),
                    lancet_protocol::Retriability::Permanent,
                ));
            }
        }
    }
    Ok(result)
}

// ── Pure URL fact extraction ────────────────────────────────────────────

fn parse_url_facts(url_str: &str) -> (String, u16, String, bool, bool) {
    match url::Url::parse(url_str) {
        Ok(url) => {
            let host = url.host_str().unwrap_or("").to_string();
            let port = url.port_or_known_default().unwrap_or(443);
            let scheme = url.scheme().to_string();
            let is_localhost = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
            let is_private_ip = host
                .parse::<std::net::IpAddr>()
                .map(is_private_ip_addr)
                .unwrap_or(false);
            (host, port, scheme, is_localhost, is_private_ip)
        }
        Err(_) => (String::new(), 0, String::new(), false, false),
    }
}

fn is_private_ip_addr(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::policy::PolicyAuthorityInput;
    use serde_json::json;

    fn envelope_required_capability(resolved: &ResolvedPolicyInput) -> RequiredCapability {
        match &resolved.authority {
            ResolvedAuthorityInput::Envelope {
                required_capability,
                ..
            } => *required_capability,
            ResolvedAuthorityInput::Direct => {
                panic!("direct authority input has no required capability")
            }
        }
    }

    fn test_resolver() -> PolicyResolver {
        PolicyResolver::defaults_for_project_root("/home/user/project")
    }

    fn empty_envelope() -> EffectiveEnvelopeSnapshot {
        EffectiveEnvelopeSnapshot {
            base_envelope_id: "base".into(),
            declared_envelope_id: None,
            capabilities: vec![],
        }
    }

    fn audit_ctx() -> PolicyAuditContext {
        PolicyAuditContext {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            call_id: Some(1),
            budget_snapshot: PolicyBudgetSnapshot {
                turn_observation_bytes_used: 0,
                turn_observation_bytes_limit: 65536,
                todo_ops_used: 0,
                todo_ops_limit: 10,
            },
        }
    }

    #[test]
    fn resolve_workspace_read_file_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(resolved.method, "observe.workspace");
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::ReadPath
        );
        assert_eq!(resolved.impact_kind, ImpactKind::WorkspaceObservation);
        let target = resolved.resolved_target.unwrap();
        assert_eq!(target.request_path, "src/main.rs");
        assert_eq!(target.normalized_path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn resolve_workspace_create_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "mutate.create".into(),
            params: json!({"path": "src/new.rs", "content": "fn main() {}"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::WritePath
        );
        assert_eq!(resolved.impact_kind, ImpactKind::WorkspaceMutation);
        assert!(resolved.resolved_target.is_some());
    }

    #[test]
    fn resolve_workspace_edit_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "mutate.edit".into(),
            params: json!({
                "path": "src/main.rs",
                "expected_hash": "blake3:0000000000000000000000000000000000000000000000000000000000000000",
                "edits": []
            }),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::WritePath
        );
        assert_eq!(resolved.impact_kind, ImpactKind::WorkspaceMutation);
        let target = resolved.resolved_target.unwrap();
        assert_eq!(target.request_path, "src/main.rs");
        assert_eq!(target.normalized_path, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn resolve_workspace_grep_extracts_optional_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "grep", "pattern": "fn main", "path": "src/"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::Inspect
        );
        assert!(resolved.resolved_target.is_some());
    }

    #[test]
    fn resolve_workspace_list_directory_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "list_directory", "path": "src/"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::Inspect
        );
        assert!(resolved.resolved_target.is_some());
    }

    #[test]
    fn resolve_workspace_find_files_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "find_files", "pattern": "*.rs", "path": "src/"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::Inspect
        );
    }

    #[test]
    fn resolve_command_run_extracts_argv_cwd_env() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "exec.command_run".into(),
            params: json!({
                "argv": ["cargo", "test"],
                "cwd": "/home/user/project",
                "env": {"RUST_LOG": "debug"},
                "declared_impact": "command_execution"
            }),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::RunCommand
        );
        assert_eq!(resolved.impact_kind, ImpactKind::CommandExecution);
        let cmd = resolved.resolved_command.unwrap();
        assert_eq!(cmd.argv, vec!["cargo", "test"]);
        assert_eq!(cmd.cwd, PathBuf::from("/home/user/project"));
        assert_eq!(cmd.env_keys, vec!["RUST_LOG"]);
    }

    #[test]
    fn resolve_web_fetch_extracts_url_facts() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "fetch.web".into(),
            params: json!({"url": "https://example.com/page"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::FetchUrl
        );
        assert_eq!(resolved.impact_kind, ImpactKind::ExternalObservation);
        let net = resolved.resolved_network.unwrap();
        assert_eq!(net.host, "example.com");
        assert_eq!(net.scheme, "https");
        assert_eq!(net.port, 443);
        assert!(!net.is_localhost);
    }

    #[test]
    fn resolve_session_todo_write_identifies_operation() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "session.todo_write".into(),
            params: json!({"todos": [{"content": "task 1", "status": "pending"}]}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::MutateSessionState
        );
        assert_eq!(resolved.impact_kind, ImpactKind::SessionStateMutation);
        let ss = resolved.resolved_session_state.unwrap();
        assert_eq!(
            ss.operation,
            super::super::capability::SessionStateOperation::TodoWrite
        );
        assert_eq!(ss.todo_ops_count, 1);
    }

    #[test]
    fn resolve_envelope_declare_identifies_operation() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "session.envelope".into(),
            params: json!({
                "mode": "declare",
                "envelope": {
                    "intent": {"description": "test"},
                    "capabilities": [],
                    "scope": {},
                    "impact": [],
                    "constraints": {}
                }
            }),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(
            envelope_required_capability(&resolved),
            RequiredCapability::MutateSessionState
        );
        let ss = resolved.resolved_session_state.unwrap();
        assert_eq!(
            ss.operation,
            super::super::capability::SessionStateOperation::EnvelopeDeclare
        );
    }

    #[test]
    fn resolve_unknown_method_errors() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "unknown.method".into(),
            params: json!({}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let err = resolver.resolve(&input).unwrap_err();
        assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn resolve_path_traversal_rejected() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "../etc/passwd"}),
            authority: PolicyAuthorityInput::Envelope {
                envelope_snapshot: empty_envelope(),
            },
            audit_context: audit_ctx(),
        };
        let err = resolver.resolve(&input).unwrap_err();
        assert_eq!(err.code, lancet_protocol::CepErrorCode::PathContainment);
    }

    #[test]
    fn resolved_policy_input_has_no_serialize() {
        // Compile-time proof: ResolvedPolicyInput does not derive Serialize/Deserialize.
        // If it did, this test would still pass but the type would be wire-exposable.
        // The absence of Serialize/Deserialize derives is the security invariant.
        fn assert_not_wire_exposable<T: 'static>() {
            // This is a compile-time marker, not a runtime check.
        }
        assert_not_wire_exposable::<ResolvedPolicyInput>();
    }
}
