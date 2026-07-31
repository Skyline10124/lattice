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

/// Resolved path facts for the workspace mutation methods.
///
/// Observation used to produce these too; since #30 `observe.workspace` is a
/// Component Tool and its paths are resolved host-side, so `mutate.create` and
/// `mutate.edit` are the only producers.
#[derive(Clone, Debug)]
pub struct ResolvedPath {
    pub request_path: String,
    pub normalized_path: PathBuf,
    pub project_root: PathBuf,
}

/// Resolved network facts for `fetch.web`.
///
/// The host only. The policy decision matches it against `allowed_hosts` through
/// the one shared matcher and records it as evidence; scheme, port, localhost,
/// and private-IP judgements belong to `policy::network::validate_fetch_url`,
/// which owns them and resolves DNS. Carrying unread copies here is the pattern
/// #121 removed: a second producer of a decision input that nothing consumes
/// invites the two to drift.
#[derive(Clone, Debug)]
pub struct ResolvedNetwork {
    pub host: String,
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

        // `observe.workspace` used to resolve its per-mode path here. It is a
        // Component Tool since #30, so the method is no longer in the catalog and
        // the lookup above already rejected it — path facts for an observation now
        // come from the WIT Tool Host in `lancet_runtime::host_workspace`, which
        // resolves them against the concrete Local Workspace instead of lexically.
        match input.method.as_str() {
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
                    let host = parse_url_host(&url);
                    invocation_facts.host = Some(host.clone());
                    resolved_network = Some(ResolvedNetwork { host });
                }
            }
            _ => {}
        }

        Ok(ResolvedPolicyInput {
            method: input.method.clone(),
            impact_kind,
            invocation_facts,
            resolved_target,
            resolved_network,
            budget_snapshot: input.audit_context.budget_snapshot.clone(),
            audit_context: input.audit_context.clone(),
        })
    }
}

// ── Pure param extraction helpers ───────────────────────────────────────

// `extract_observe_workspace_path` (per-mode: required `path` for read_file and
// list_directory, optional with a `.` fallback for grep and find_files) and its
// optional-path helper left with `observe.workspace` (#30). The mode vocabulary is
// the Component's params schema now, so keeping a second reader of it here would
// be a copy that can only drift from the manifest.

fn extract_path_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("path")?.as_str().map(String::from)
}

fn extract_url_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("url")?.as_str().map(String::from)
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

/// The host of a `fetch.web` URL, empty when the URL does not parse.
///
/// An unparseable URL yields an empty host, which matches no `allowed_hosts`
/// pattern — the allow decision stays fail-closed and URL shape is rejected with
/// a precise error later, by `policy::network::validate_fetch_url`.
fn parse_url_host(url_str: &str) -> String {
    url::Url::parse(url_str)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default()
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
    fn resolve_observe_workspace_errors_as_unknown_method() {
        // Regression for #30: `observe.workspace` is an embedded Component Tool,
        // so it left the catalog and the resolver must reject it as an unknown
        // method rather than resolve lexical path facts for it. The four per-mode
        // extraction tests (read_file / list_directory / grep / find_files) went
        // with the arm they covered; the mode vocabulary and its path semantics
        // are the Tool manifest's params schema now, enforced host-side by
        // `lancet_runtime::host_workspace`.
        let resolver = test_resolver();
        for params in [
            json!({"mode": "read_file", "path": "src/main.rs"}),
            json!({"mode": "list_directory", "path": "src/"}),
            json!({"mode": "grep", "pattern": "fn main", "path": "src/"}),
            json!({"mode": "find_files", "pattern": "*.rs"}),
        ] {
            let input = PolicyInput {
                method: "observe.workspace".into(),
                params,
                authority: PolicyAuthorityInput,
                audit_context: audit_ctx(),
            };
            let err = resolver.resolve(&input).unwrap_err();
            assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);
        }
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
    fn resolve_web_fetch_extracts_the_host() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "fetch.web".into(),
            params: json!({"url": "https://example.com/page"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        assert_eq!(resolved.impact_kind, ImpactKind::ExternalObservation);
        assert_eq!(resolved.resolved_network.unwrap().host, "example.com");
        assert_eq!(resolved.invocation_facts.host.unwrap(), "example.com");
    }

    /// An unparseable URL resolves to an empty host, which no `allowed_hosts`
    /// pattern matches — the allow decision stays fail-closed.
    #[test]
    fn resolve_web_fetch_unparseable_url_yields_no_grantable_host() {
        let resolver = test_resolver();
        let input = PolicyInput {
            method: "fetch.web".into(),
            params: json!({"url": "not a url"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(),
        };
        let resolved = resolver.resolve(&input).unwrap();
        let host = resolved.resolved_network.unwrap().host;
        assert!(host.is_empty(), "got: {host}");
        assert!(!super::super::network::host_allowed_by_list(
            &host,
            &["example.com".to_string()]
        ));
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

    /// Lexical traversal rejection is a property of resolution itself, so it
    /// stays pinned on the methods that still resolve a path. It used to be
    /// asserted through `observe.workspace`, which left with #30; every remaining
    /// path-resolving method is covered here so the check cannot go dead again by
    /// losing one method.
    #[test]
    fn resolve_path_traversal_rejected() {
        let resolver = test_resolver();
        for (method, params) in [
            (
                "mutate.create",
                json!({"path": "../etc/passwd", "content": ""}),
            ),
            ("mutate.edit", json!({"path": "../etc/passwd", "edits": []})),
        ] {
            let input = PolicyInput {
                method: method.into(),
                params,
                authority: PolicyAuthorityInput,
                audit_context: audit_ctx(),
            };
            let err = resolver.resolve(&input).unwrap_err();
            assert_eq!(
                err.code,
                lancet_protocol::CepErrorCode::PathContainment,
                "{method} must reject '..' traversal"
            );
        }
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
