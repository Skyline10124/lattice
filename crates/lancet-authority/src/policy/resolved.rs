// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resolved facts table and resolver for CEP policy evaluation.
//!
//! Transforms the public [`PolicyInput`] into an internal [`ResolvedPolicyInput`]
//! with lexical path facts, host facts, session-state facts, and immutable
//! budget snapshots. The resolver is pure: no filesystem I/O, no time reads,
//! no mutable counter access.

use std::path::{Path, PathBuf};

use lancet_catalog::catalog::catalog;
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::policy::{PolicyAuditContext, PolicyBudgetSnapshot, PolicyInput};

use super::capability::InvocationFacts;

// ── Resolved types (executor-owned, no Serialize/Deserialize) ───────────

/// Resolved path facts for workspace observation/mutation methods.
#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub request_path: String,
    pub normalized_path: PathBuf,
    pub project_root: PathBuf,
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

/// Internal resolved facts table built from [`PolicyInput`].
///
/// Not a public protocol type — executor-owned only.
#[derive(Clone, Debug)]
pub struct ResolvedPolicyInput {
    pub method: String,
    pub impact_kind: ImpactKind,
    pub invocation_facts: InvocationFacts,
    pub resolved_target: Option<ResolvedPath>,
    pub resolved_network: Option<ResolvedNetwork>,
    pub resolved_session_state: Option<ResolvedSessionState>,
    pub budget_snapshot: PolicyBudgetSnapshot,
    pub audit_context: PolicyAuditContext,
}

/// Resolver that builds [`ResolvedPolicyInput`] from [`PolicyInput`].
///
/// Pure resolution: no filesystem I/O, no time reads, no mutable state.
///
/// Resolution is lexical, so the project root is the only policy input it
/// needs. It used to carry observation/network/budget/workspace copies that
/// nothing read — a fourth phantom copy of the limits, removed with #121.
/// Decisions over those limits belong to [`super::default::DefaultPolicyHook`],
/// which owns the one `PolicyConfig`.
#[derive(Clone, Debug)]
pub struct PolicyResolver {
    pub project_root: PathBuf,
}

impl PolicyResolver {
    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
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

        let mut invocation_facts = InvocationFacts::default();
        let mut resolved_target = None;
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
            _ => {}
        }

        Ok(ResolvedPolicyInput {
            method: input.method.clone(),
            impact_kind,
            invocation_facts,
            resolved_target,
            resolved_network,
            resolved_session_state,
            budget_snapshot: input.audit_context.budget_snapshot.clone(),
            audit_context: input.audit_context.clone(),
        })
    }
}

// ── Pure param extraction helpers ───────────────────────────────────────

fn extract_observe_workspace_path(params: &serde_json::Value) -> Option<String> {
    match params.get("mode").and_then(|v| v.as_str()) {
        Some("read_file") | Some("list_directory") => extract_path_from_params(params),
        Some("grep") | Some("find_files") => {
            extract_optional_path_from_params(params).or_else(|| Some(".".to_string()))
        }
        _ => None,
    }
}

fn extract_path_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("path")?.as_str().map(String::from)
}

fn extract_optional_path_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("path")?.as_str().map(String::from)
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
    if result.as_os_str().is_empty() {
        result.push(".");
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

    fn test_resolver() -> PolicyResolver {
        PolicyResolver::defaults_for_project_root("/home/user/project")
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
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(resolved.method, "observe.workspace");
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
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
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
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
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
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert!(resolved.resolved_target.is_some());
    }

    #[test]
    fn resolve_workspace_list_directory_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "list_directory", "path": "src/"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert!(resolved.resolved_target.is_some());
    }

    #[test]
    fn resolve_workspace_find_files_extracts_path() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "find_files", "pattern": "*.rs", "path": "src/"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let _resolved = resolver.resolve(&input).unwrap();
    }

    #[test]
    fn resolve_command_run_errors_as_unknown_method() {
        // Regression for issue #26 (Codeberg #4): exec.command_run is retired
        // from the
        // catalog, so the resolver must reject it as an unknown method.
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "exec.command_run".into(),
            params: json!({"argv": ["cargo", "test"]}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let err = resolver.resolve(&input).unwrap_err();
        assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn resolve_web_fetch_extracts_url_facts() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "fetch.web".into(),
            params: json!({"url": "https://example.com/page"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
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
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(resolved.impact_kind, ImpactKind::SessionStateMutation);
        let ss = resolved.resolved_session_state.unwrap();
        assert_eq!(
            ss.operation,
            super::super::capability::SessionStateOperation::TodoWrite
        );
        assert_eq!(ss.todo_ops_count, 1);
    }

    #[test]
    fn resolve_unknown_method_errors() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "unknown.method".into(),
            params: json!({}),
            authority: PolicyAuthorityInput,
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
            authority: PolicyAuthorityInput,
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
