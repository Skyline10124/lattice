// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resolved facts table and resolver for CEP policy evaluation.
//!
//! Transforms the public [`PolicyInput`] into an internal [`ResolvedPolicyInput`]
//! with lexical path facts, host facts, session-state facts, and immutable
//! budget snapshots. The resolver is pure: no filesystem I/O, no time reads,
//! no mutable counter access.

use std::path::PathBuf;

use lancet_catalog::catalog::catalog;
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::policy::{PolicyAuditContext, PolicyBudgetSnapshot, PolicyInput};

use super::capability::InvocationFacts;

// ── Resolved types (executor-owned, no Serialize/Deserialize) ───────────

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
        let mut resolved_network = None;

        // Observation and mutation paths are resolved by their Component Tool
        // Hosts. Those methods are absent from the legacy catalog, so the lookup
        // above rejects them before this branch and there is no parallel lexical
        // path-authority path here.
        if input.method == "fetch.web"
            && let Some(url) = extract_url_from_params(&input.params)
        {
            let host = parse_url_host(&url);
            invocation_facts.host = Some(host.clone());
            resolved_network = Some(ResolvedNetwork { host });
        }

        Ok(ResolvedPolicyInput {
            method: input.method.clone(),
            impact_kind,
            invocation_facts,
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

fn extract_url_from_params(params: &serde_json::Value) -> Option<String> {
    params.get("url")?.as_str().map(String::from)
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
            invocation_id: Some(
                lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                    .expect("invocation id"),
            ),
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
    fn resolve_component_methods_as_unknown_legacy_methods() {
        let resolver = test_resolver();
        for (method, params) in [
            (
                "mutate.create",
                json!({"path": "src/new.rs", "content": "fn main() {}"}),
            ),
            (
                "mutate.edit",
                json!({"path": "src/main.rs", "expected_hash": "blake3:00", "edits": []}),
            ),
            ("fetch.web", json!({"url": "https://example.com/"})),
        ] {
            let input = PolicyInput {
                method: method.into(),
                params,
                authority: PolicyAuthorityInput,
                audit_context: audit_ctx(),
            };
            let err = resolver
                .resolve(&input)
                .expect_err("Component methods must not resolve through the legacy catalog");
            assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);
        }
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
