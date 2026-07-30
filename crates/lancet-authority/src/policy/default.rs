// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default in-process policy hook implementation.
//!
//! Combines capability matching, network policy, and budget enforcement into a
//! single data-driven pure decision function.
//!
//! **Design constraints:**
//! - `PolicyHook::decide` is synchronous, deterministic, and performs no I/O.
//! - `decide_resolved` is a pure function on resolved facts.
//! - Policy is data-driven (TOML configuration), not hard-coded if/else.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lancet_protocol::policy::{
    PolicyActionRequired, PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode,
};

use super::network::NetworkPolicyConfig;
use super::observation::ObservationPolicyConfig;
use super::resolved::PolicyResolver;
use lancet_protocol::effect_types::ImpactKind;

// ── Method policy overrides ────────────────────────────────────────────

/// Fixed-field overrides for a single method.
///
/// Only fixed fields are allowed — this must not become a second policy engine.
/// Every field here must have a live reader on the execution side (#121): a
/// configurable key with no reader is a silently ineffective policy, which is
/// worse than an unsupported key that errors. `enabled` is read in
/// [`DefaultPolicyHook::decide_resolved_direct`] and `max_todo_ops` clamps the
/// turn-scoped todo budget in [`DefaultPolicyHook::budget_denial`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MethodPolicy {
    pub enabled: Option<bool>,
    pub max_todo_ops: Option<usize>,
}

// ── Workspace policy (write blacklist + read visibility) ──────────────

/// Immutable workspace policy for write-deny and read-visibility rules.
///
/// Write policy is a blacklist: a path is allowed unless it matches any
/// denied write pattern. Read visibility is decided by
/// [`super::visibility::ReadVisibilityPolicy`] built from the read fields
/// below plus runtime System Path Exclusions and Git status.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspacePolicy {
    pub denied_write_patterns: Vec<String>,
    pub included_ignored_read_patterns: Vec<String>,
    pub denied_read_patterns: Vec<String>,
}

impl WorkspacePolicy {
    /// Validate every configured pattern using Guard L1 write-pattern rules.
    pub fn validate(&self) -> Result<(), String> {
        use crate::guard::normalize_write_path_pattern;
        for (field, patterns) in [
            ("denied_write_patterns", &self.denied_write_patterns),
            (
                "included_ignored_read_patterns",
                &self.included_ignored_read_patterns,
            ),
            ("denied_read_patterns", &self.denied_read_patterns),
        ] {
            for pattern in patterns {
                if pattern.is_empty() {
                    return Err(format!("{field} entry must not be empty"));
                }
                if pattern.contains('\0') {
                    return Err(format!("{field} entry must not contain NUL bytes"));
                }
                if pattern.contains("..") {
                    return Err(format!("{field} entry must not contain '..' traversal"));
                }
                if Path::new(pattern).is_absolute() {
                    return Err(format!("{field} entry must be project-root-relative"));
                }
                normalize_write_path_pattern(pattern).map_err(|e| e.message)?;
            }
        }
        Ok(())
    }

    /// Build the pure read-visibility decision surface for this policy.
    pub fn read_visibility_policy(
        &self,
    ) -> Result<super::visibility::ReadVisibilityPolicy, String> {
        super::visibility::ReadVisibilityPolicy::from_patterns(
            &self.included_ignored_read_patterns,
            &self.denied_read_patterns,
        )
    }

    /// Build the post-policy authorized region for a method decision.
    pub fn authorized_region_for(
        &self,
        method: &str,
        policy: &super::CepPolicyConfig,
    ) -> crate::effect::region::AuthorizedEffectRegion {
        use crate::effect::region::AuthorizedEffectRegion;
        use lancet_protocol::effect_types::ImpactKind;

        let mut region = AuthorizedEffectRegion::empty();
        match method {
            "observe.workspace" => {
                region.kinds.insert(ImpactKind::WorkspaceObservation);
                // Observation effects are project-relative; authorize the workspace root sentinel.
                region.observation_roots = vec![PathBuf::from(".")];
            }
            "fetch.web" => {
                region.kinds.insert(ImpactKind::ExternalObservation);
                // Normalize at the construction site (#122) so the region carries
                // comparison-form patterns; `contains` applies the same shared
                // matcher the network/policy layers use.
                region.hosts = policy
                    .network
                    .allowed_hosts
                    .iter()
                    .map(|host| super::network::normalize_host_pattern(host))
                    .collect();
            }
            "mutate.create" | "mutate.edit" => {
                region.kinds.insert(ImpactKind::WorkspaceMutation);
                // System Path Exclusions are host-enforced and not
                // profile-configurable (AGENTS.md / ADR 0003).
                for exclusion in super::visibility::SYSTEM_PATH_EXCLUSIONS {
                    region
                        .denied_write_path_prefixes
                        .push(PathBuf::from(exclusion));
                }
                apply_denied_write_patterns(
                    &mut region,
                    &self.denied_write_patterns,
                    &policy.project_root,
                );
            }
            _ => {}
        }
        region
    }
}

fn apply_denied_write_patterns(
    region: &mut crate::effect::region::AuthorizedEffectRegion,
    patterns: &[String],
    _project_root: &Path,
) {
    use crate::guard::{WritePathPattern, normalize_write_path_pattern};
    for pattern in patterns {
        if let Ok(parsed) = normalize_write_path_pattern(pattern) {
            match parsed {
                WritePathPattern::ExactFile(path) => region.denied_write_paths.push(path),
                WritePathPattern::DirectoryGlob(path) => {
                    region.denied_write_path_prefixes.push(path)
                }
            }
        }
    }
}

// ── Unified policy config ──────────────────────────────────────────────

/// Unified, data-driven policy configuration.
///
/// Merge precedence: compiled defaults < profile inline `[lancet.*]` <
/// external policy file < test/runtime override.
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    pub methods: BTreeMap<String, MethodPolicy>,
    pub network: NetworkPolicyConfig,
    /// The one observation limit set (#121); the turn-scoped budget is derived
    /// from it through [`PolicyConfig::budget`], never stored twice.
    pub observation: ObservationPolicyConfig,
    pub workspace: WorkspacePolicy,
    pub project_root: PathBuf,
    pub denied_effect_kinds: BTreeSet<ImpactKind>,
}

impl PolicyConfig {
    /// Compiled defaults for the given project root.
    pub fn defaults_for_project_root(project_root: PathBuf) -> Self {
        Self {
            methods: BTreeMap::new(),
            network: NetworkPolicyConfig::safe_defaults(),
            observation: ObservationPolicyConfig::defaults(),
            workspace: WorkspacePolicy::default(),
            project_root,
            denied_effect_kinds: BTreeSet::new(),
        }
    }

    /// Merge an external TOML policy file into this config.
    ///
    /// `[budget]` is a deprecated alias of `[observation]`: both tables write
    /// the one limit set (#121) so no external key can land in a slot nothing
    /// reads. `[observation]` is applied last, so it wins a conflict.
    pub fn merge_external(&mut self, external: &ExternalPolicyFile) -> Result<(), String> {
        if let Some(methods) = &external.methods {
            for (name, ext_method) in methods {
                let policy = MethodPolicy {
                    enabled: ext_method.enabled,
                    max_todo_ops: ext_method.max_todo_ops,
                };
                self.methods.insert(name.clone(), policy);
            }
        }
        if let Some(net) = &external.network {
            if let Some(v) = net.allow_public_https {
                self.network.allow_public_https = v;
            }
            if let Some(v) = net.allow_localhost {
                self.network.allow_localhost = v;
            }
            if let Some(v) = &net.allowed_hosts {
                self.network.allowed_hosts = v.clone();
            }
            if let Some(v) = net.max_web_response_bytes {
                self.network.max_web_response_bytes = v;
            }
        }
        // `[budget]` is the deprecated alias, applied before `[observation]` so
        // the canonical table wins a conflict (#121).
        if let Some(budget) = &external.budget {
            apply_external_limits(&mut self.observation, budget);
        }
        if let Some(effects) = &external.effects
            && let Some(deny) = &effects.deny
        {
            self.denied_effect_kinds = deny.iter().cloned().collect();
        }
        if let Some(workspace) = &external.workspace {
            if let Some(patterns) = &workspace.denied_write_patterns {
                self.workspace.denied_write_patterns = patterns.clone();
            }
            if let Some(patterns) = &workspace.included_ignored_read_patterns {
                self.workspace.included_ignored_read_patterns = patterns.clone();
            }
            if let Some(patterns) = &workspace.denied_read_patterns {
                self.workspace.denied_read_patterns = patterns.clone();
            }
            self.workspace.validate()?;
        }
        if let Some(obs) = &external.observation {
            apply_external_limits(&mut self.observation, obs);
        }
        Ok(())
    }

    /// Construct a `PolicyResolver` from this config.
    ///
    /// Resolution is lexical, so only the project root crosses over; the limits
    /// stay in this config, where the decision path reads them (#121).
    pub fn to_resolver(&self) -> PolicyResolver {
        PolicyResolver {
            project_root: self.project_root.clone(),
        }
    }

    /// Construct a `CepPolicyConfig` for backward compatibility with
    /// existing CEP handlers.
    ///
    /// This config carries no Component Tool admission inputs, so the result
    /// admits nothing (#28 fail-closed default); callers that dispatch
    /// Component Tools set `capability_admission` from the operator profile.
    pub fn to_cep_policy_config(&self) -> super::CepPolicyConfig {
        super::CepPolicyConfig {
            project_root: self.project_root.clone(),
            observation: self.observation.clone(),
            network: self.network.clone(),
            denied_effect_kinds: self.denied_effect_kinds.clone(),
            capability_admission: Default::default(),
        }
    }
}

// ── External TOML policy file DTOs ─────────────────────────────────────

/// External TOML policy file structure.
///
/// Every key here reaches an execution-side reader (#121). `deny_unknown_fields`
/// makes a retired key a hard parse error rather than a silently dropped
/// setting, so operators learn that a section stopped existing.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPolicyFile {
    pub methods: Option<BTreeMap<String, ExternalMethodPolicy>>,
    pub network: Option<ExternalNetworkPolicy>,
    pub budget: Option<ExternalBudgetPolicy>,
    pub observation: Option<ExternalObservationPolicy>,
    pub effects: Option<ExternalEffectsPolicy>,
    pub workspace: Option<ExternalWorkspacePolicy>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalWorkspacePolicy {
    pub denied_write_patterns: Option<Vec<String>>,
    pub included_ignored_read_patterns: Option<Vec<String>>,
    pub denied_read_patterns: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalMethodPolicy {
    pub enabled: Option<bool>,
    pub max_todo_ops: Option<usize>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalNetworkPolicy {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    pub allowed_hosts: Option<Vec<String>>,
    pub max_web_response_bytes: Option<usize>,
}

/// Deprecated `[budget]` alias of `[observation]`.
///
/// Kept as an alias so existing policy files keep loading. It is the same type,
/// so the two tables cannot drift apart in schema either (#121).
pub type ExternalBudgetPolicy = ExternalObservationPolicy;

/// Optional overrides for the one observation limit set.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalObservationPolicy {
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

/// Apply the present keys of one external limits table onto the live config.
///
/// Both `[budget]` and `[observation]` route through here, so a key can never
/// be accepted by one table and dropped by the other (#121).
fn apply_external_limits(
    observation: &mut ObservationPolicyConfig,
    external: &ExternalObservationPolicy,
) {
    if let Some(v) = external.max_single_read_bytes {
        observation.max_single_read_bytes = v;
    }
    if let Some(v) = external.max_turn_observation_bytes {
        observation.max_turn_observation_bytes = v;
    }
    if let Some(v) = external.max_grep_matches {
        observation.max_grep_matches = v;
    }
    if let Some(v) = external.max_list_entries {
        observation.max_list_entries = v;
    }
    if let Some(v) = external.max_find_paths {
        observation.max_find_paths = v;
    }
    if let Some(v) = external.max_todo_ops {
        observation.max_todo_ops = v;
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEffectsPolicy {
    #[serde(default)]
    pub deny: Option<Vec<ImpactKind>>,
}

// ── Rich outcome and evidence (executor-owned) ─────────────────────────

/// Rich policy outcome with executor-owned evidence for audit.
#[derive(Clone, Debug)]
pub struct RichPolicyOutcome {
    pub decision: PolicyDecision,
    pub evidence: PolicyEvidence,
}

/// Executor-owned policy evidence for audit metadata.
///
/// Contains rich facts that the public `PolicyDecision` does not expose.
#[derive(Clone, Debug, Default)]
pub struct PolicyEvidence {
    pub network_host: Option<String>,
    pub budget_remaining: Option<u64>,
    pub resolved_paths: Vec<String>,
}

// ── Default policy hook ────────────────────────────────────────────────

/// Default in-process policy hook.
///
/// Combines capability matching, network policy, and budget enforcement into a
/// single data-driven pure decision function.
///
/// **Invariant:** `decide()` performs no I/O, no time reads, no mutable
/// budget access, and no filesystem/network/clock/environment reads.
#[derive(Clone, Debug)]
pub struct DefaultPolicyHook {
    config: PolicyConfig,
    resolver: PolicyResolver,
}

impl DefaultPolicyHook {
    /// Create a new hook from the given policy config.
    pub fn new(config: PolicyConfig) -> Self {
        let resolver = config.to_resolver();
        Self { config, resolver }
    }

    /// Immutable policy configuration backing this hook.
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Executor-owned helper: returns `RichPolicyOutcome` with evidence for audit.
    ///
    /// This is not part of the public `PolicyHook` trait.
    pub fn decide_with_evidence(&self, input: &PolicyInput) -> RichPolicyOutcome {
        match self.resolver.resolve(input) {
            Ok(resolved) => self.decide_resolved(&resolved),
            Err(err) => RichPolicyOutcome {
                decision: PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::NonDeterminable,
                    message: err.message,
                    action_required: None,
                },
                evidence: PolicyEvidence::default(),
            },
        }
    }

    /// Pure decision function on resolved facts.
    ///
    /// Policy: enabled-method → denied-effect → network → budget → workspace write region.
    fn decide_resolved(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
    ) -> RichPolicyOutcome {
        self.decide_resolved_direct(resolved)
    }

    fn decide_resolved_direct(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
    ) -> RichPolicyOutcome {
        let method_policy = self.config.methods.get(&resolved.method);
        if let Some(mp) = method_policy
            && mp.enabled == Some(false)
        {
            return RichPolicyOutcome {
                decision: PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::PolicyConfigInvalid,
                    message: format!("method '{}' disabled by policy config", resolved.method),
                    action_required: None,
                },
                evidence: PolicyEvidence::default(),
            };
        }

        if self
            .config
            .denied_effect_kinds
            .contains(&resolved.impact_kind)
        {
            return RichPolicyOutcome {
                decision: PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::PolicyConfigInvalid,
                    message: format!(
                        "impact kind '{:?}' denied by policy config",
                        resolved.impact_kind
                    ),
                    action_required: None,
                },
                evidence: PolicyEvidence::default(),
            };
        }

        // No allowlist gate for mutate.* — blacklist semantics mean an empty
        // denied_write_patterns allows all workspace paths. Per-path deny
        // enforcement happens in Guard L1 / validate_path_facts.

        if let Some(denied) = self.check_network_and_budget(resolved) {
            return denied;
        }

        self.allowed_outcome_direct(resolved)
    }

    fn check_network_and_budget(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
    ) -> Option<RichPolicyOutcome> {
        if resolved.method == "fetch.web"
            && let Some(net) = &resolved.resolved_network
            && !self.config.network.allowed_hosts.is_empty()
        {
            // Single shared matcher (#122): policy, network validation, and the
            // effect region must agree on `allowed_hosts` semantics.
            let allowed =
                super::network::host_allowed_by_list(&net.host, &self.config.network.allowed_hosts);
            if !allowed {
                return Some(RichPolicyOutcome {
                    decision: PolicyDecision::Denied {
                        reason_code: PolicyReasonCode::NetworkDenied,
                        message: format!("host '{}' not in allowed_hosts", net.host),
                        action_required: Some(PolicyActionRequired::ChangeUrl),
                    },
                    evidence: PolicyEvidence {
                        network_host: Some(net.host.clone()),
                        ..Default::default()
                    },
                });
            }
        }

        let budget = &resolved.budget_snapshot;
        let is_observation = matches!(resolved.method.as_str(), "observe.workspace" | "fetch.web");
        if is_observation
            && budget.turn_observation_bytes_used >= budget.turn_observation_bytes_limit
        {
            return Some(RichPolicyOutcome {
                decision: PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::BudgetExceeded,
                    message: "turn observation budget exhausted".into(),
                    action_required: Some(PolicyActionRequired::ReduceReadSize),
                },
                evidence: PolicyEvidence {
                    budget_remaining: Some(
                        budget
                            .turn_observation_bytes_limit
                            .saturating_sub(budget.turn_observation_bytes_used),
                    ),
                    ..Default::default()
                },
            });
        }
        // The per-turn todo-ops budget check lived here. `session.todo_write` is
        // a Component Tool since #29, so no native method reaches this branch;
        // see FINDINGS.md for the residual (Component result accounting measures
        // but does not charge, per #27/#28).
        None
    }

    fn allowed_outcome_direct(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
    ) -> RichPolicyOutcome {
        let budget = &resolved.budget_snapshot;
        let mut resolved_paths = Vec::new();
        if let Some(target) = &resolved.resolved_target {
            resolved_paths.push(target.request_path.clone());
        }
        RichPolicyOutcome {
            decision: PolicyDecision::Allowed,
            evidence: PolicyEvidence {
                network_host: resolved.resolved_network.as_ref().map(|n| n.host.clone()),
                budget_remaining: Some(
                    budget
                        .turn_observation_bytes_limit
                        .saturating_sub(budget.turn_observation_bytes_used),
                ),
                resolved_paths,
            },
        }
    }
}

impl PolicyHook for DefaultPolicyHook {
    fn decide(&self, input: &PolicyInput) -> PolicyDecision {
        self.decide_with_evidence(input).decision
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::BudgetLimits;
    use super::*;
    use lancet_protocol::policy::{PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot};
    use serde_json::json;

    fn test_config() -> PolicyConfig {
        PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"))
    }

    fn audit_ctx(budget: PolicyBudgetSnapshot) -> PolicyAuditContext {
        PolicyAuditContext {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            call_id: Some(1),
            budget_snapshot: budget,
        }
    }

    fn default_budget() -> PolicyBudgetSnapshot {
        PolicyBudgetSnapshot {
            turn_observation_bytes_used: 0,
            turn_observation_bytes_limit: 65536,
            todo_ops_used: 0,
            todo_ops_limit: 10,
        }
    }

    // ── Test 1: observe.workspace read_file allowed ──────

    #[test]
    fn default_hook_allows_workspace_read_file() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert_eq!(outcome.decision, PolicyDecision::Allowed);
    }

    // ── Test 2: mutate.create with no denied_write_patterns (blacklist allows all) ─────────

    #[test]
    fn default_hook_allows_create_with_no_denied_patterns() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "mutate.create".into(),
            params: json!({"path": "src/new.rs", "content": "fn main() {}"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert_eq!(outcome.decision, PolicyDecision::Allowed);
    }

    // ── Test 3: exec.command_run is retired and rejected as unknown method ──

    #[test]
    fn default_hook_denies_command_run_as_unknown_method() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "exec.command_run".into(),
            params: json!({
                "argv": ["sh", "-c", "rm -rf /"],
            }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        // exec.command_run is retired from the catalog, so the resolver
        // rejects it as NonDeterminable before any command policy check.
        assert!(matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::NonDeterminable,
                ..
            }
        ));
    }

    // ── Test 4: fetch.web with disallowed host ─────────────────────────

    #[test]
    fn default_hook_denies_web_fetch_with_disallowed_host() {
        let mut config = test_config();
        config.network.allowed_hosts = vec!["allowed.example".into()];
        config.network.allow_public_https = false;
        let hook = DefaultPolicyHook::new(config);
        let input = PolicyInput {
            method: "fetch.web".into(),
            params: json!({"url": "https://evil.example.com/page"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert!(matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::NetworkDenied,
                ..
            }
        ));
    }

    /// #122: the hook honours the same non-canonical `allowed_hosts` forms the
    /// matcher does, and the region it hands to the effect checker stores the
    /// comparison form — so a leading-dot entry cannot be allowed here and then
    /// rejected as an `EffectViolation`.
    #[test]
    fn default_hook_and_region_agree_on_non_canonical_allowed_hosts() {
        for pattern in [".example.com", "Example.com", " example.com "] {
            let mut config = test_config();
            config.network.allowed_hosts = vec![pattern.to_string()];
            config.network.allow_public_https = false;
            let policy = config.to_cep_policy_config();
            let region = config.workspace.authorized_region_for("fetch.web", &policy);
            for host in ["example.com", "api.example.com"] {
                let granted = super::super::network::host_allowed_by_list(
                    host,
                    &config.network.allowed_hosts,
                );
                let region_grants = region.contains(&crate::effect::ledger::ObservedEffect {
                    kind: ImpactKind::ExternalObservation,
                    scope: crate::effect::ledger::ObservedScope::Host(host.to_string()),
                    required_capability: lancet_catalog::method::RequiredCapability::FetchUrl,
                    source: crate::effect::ledger::EffectSource::HandlerBackend,
                });
                assert_eq!(
                    granted, region_grants,
                    "pattern {pattern:?} host {host}: policy={granted} region={region_grants}"
                );
            }

            let hook = DefaultPolicyHook::new(config);
            let outcome = hook.decide_with_evidence(&PolicyInput {
                method: "fetch.web".into(),
                params: json!({"url": "https://api.example.com/v1"}),
                authority: PolicyAuthorityInput,
                audit_context: audit_ctx(default_budget()),
            });
            let allowed = matches!(outcome.decision, PolicyDecision::Allowed);
            // Only the leading-dot form covers the subdomain; the exact forms
            // must deny it. Either way the region above agreed.
            assert_eq!(
                allowed,
                pattern.trim().starts_with('.'),
                "pattern {pattern:?} produced decision {:?}",
                outcome.decision
            );
        }
    }

    /// The region carries patterns in comparison form, not the operator's
    /// literal profile text (#122).
    #[test]
    fn authorized_region_stores_normalized_hosts() {
        let mut config = test_config();
        config.network.allowed_hosts = vec![" Example.COM ".into(), ".Example.com".into()];
        let policy = config.to_cep_policy_config();
        let region = config.workspace.authorized_region_for("fetch.web", &policy);
        assert_eq!(region.hosts, vec!["example.com", ".example.com"]);
    }

    // ── Test 5: observation over budget ────────────────────────────────

    #[test]
    fn default_hook_denies_observation_over_budget() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(PolicyBudgetSnapshot {
                turn_observation_bytes_used: 65536,
                turn_observation_bytes_limit: 65536,
                todo_ops_used: 0,
                todo_ops_limit: 10,
            }),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert!(matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::BudgetExceeded,
                ..
            }
        ));
    }

    // ── Test 6: Denied for unknown method ──────────────────

    #[test]
    fn default_hook_returns_denied_for_unknown_method() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "unknown.method".into(),
            params: json!({}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        // Unknown method → resolver fails → NonDeterminable
        assert!(matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::NonDeterminable,
                ..
            }
        ));
    }

    // ── Test 7: PolicyConfig::defaults_for_project_root ────────────────

    #[test]
    fn policy_config_defaults_produces_valid_config() {
        let config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));
        assert!(config.network.allow_public_https);
        assert!(config.observation.max_single_read_bytes > 0);
        // The budget is a derived projection, not a second config surface.
        assert_eq!(
            config.observation.budget_limits(),
            BudgetLimits::defaults(),
            "compiled defaults must agree between the limit set and its projection"
        );
    }

    // ── Test 8: PolicyConfig merge precedence ──────────────────────────

    #[test]
    fn policy_config_merge_precedence_external_overrides_defaults() {
        let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));

        let toml_str = r#"
[network]
allow_public_https = false
"#;
        let external: ExternalPolicyFile = toml::from_str(toml_str).unwrap();
        config.merge_external(&external).unwrap();

        assert!(!config.network.allow_public_https);
    }

    /// #121: an external `[budget]` per-op cap must reach the field the
    /// handlers actually read (`observation.*`). It used to land in a second
    /// `BudgetLimits` copy that no per-op reader consulted.
    #[test]
    fn external_budget_table_reaches_the_per_op_readers() {
        let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));

        let external: ExternalPolicyFile = toml::from_str(
            r#"
[budget]
max_single_read_bytes = 4096
max_grep_matches = 7
max_list_entries = 8
max_find_paths = 9
"#,
        )
        .unwrap();
        config.merge_external(&external).unwrap();

        assert_eq!(config.observation.max_single_read_bytes, 4096);
        assert_eq!(config.observation.max_grep_matches, 7);
        assert_eq!(config.observation.max_list_entries, 8);
        assert_eq!(config.observation.max_find_paths, 9);
    }

    /// #121: an external `[observation]` turn cap must reach the turn-scoped
    /// tracker. It used to be dropped because only `[budget]` wrote
    /// `self.budget` and nothing resynced afterwards.
    #[test]
    fn external_observation_turn_caps_reach_the_budget_projection() {
        let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));

        let external: ExternalPolicyFile = toml::from_str(
            r#"
[observation]
max_turn_observation_bytes = 2048
max_todo_ops = 3
"#,
        )
        .unwrap();
        config.merge_external(&external).unwrap();

        let budget = config.to_cep_policy_config().budget();
        assert_eq!(budget.max_turn_observation_bytes, 2048);
        assert_eq!(budget.max_todo_ops, 3);
        assert_eq!(budget, config.observation.budget_limits());
    }

    /// #121: `[budget]` is a deprecated alias applied before `[observation]`,
    /// so `[observation]` wins a conflict and the two never diverge.
    #[test]
    fn external_observation_wins_over_the_budget_alias() {
        let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));

        let external: ExternalPolicyFile = toml::from_str(
            r#"
[budget]
max_todo_ops = 11
max_grep_matches = 11

[observation]
max_todo_ops = 22
"#,
        )
        .unwrap();
        config.merge_external(&external).unwrap();

        assert_eq!(config.observation.max_todo_ops, 22);
        assert_eq!(config.to_cep_policy_config().budget().max_todo_ops, 22);
        // A key only the alias sets still applies.
        assert_eq!(config.observation.max_grep_matches, 11);
    }

    /// #121: the retired `[capability_sources]` table and the three retired
    /// `[methods.*]` keys are hard parse errors, not silently ignored settings.
    #[test]
    fn retired_external_policy_keys_fail_to_parse() {
        for retired in [
            "[capability_sources]\nobservation = [\"base\"]\n",
            "[methods.\"observe.workspace\"]\nallowed_sources = [\"declared\"]\n",
            "[methods.\"observe.workspace\"]\nrequired_capability = \"ReadPath\"\n",
            "[methods.\"observe.workspace\"]\nmax_single_read_bytes = 1024\n",
        ] {
            let parsed = toml::from_str::<ExternalPolicyFile>(retired);
            assert!(
                parsed.is_err(),
                "retired external policy key must be rejected: {retired}"
            );
        }
    }

    // ── Test 9: decide_resolved is pure ────────────────────────────────

    #[test]
    fn decide_resolved_is_pure_same_input_same_output() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome1 = hook.decide_with_evidence(&input);
        let outcome2 = hook.decide_with_evidence(&input);
        assert_eq!(outcome1.decision, outcome2.decision);
        assert_eq!(
            outcome1.evidence.resolved_paths,
            outcome2.evidence.resolved_paths
        );
    }

    // ── Test 10: decide_with_evidence contains rich facts ──────────────

    #[test]
    fn decide_with_evidence_contains_rich_facts() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert_eq!(outcome.decision, PolicyDecision::Allowed);
        assert!(outcome.evidence.budget_remaining.is_some());
        assert!(!outcome.evidence.resolved_paths.is_empty());
    }

    // ── Additional: PolicyHook::decide returns stable PolicyDecision ───

    #[test]
    fn policy_hook_decide_returns_stable_decision() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "src/main.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let decision = hook.decide(&input);
        assert_eq!(decision, PolicyDecision::Allowed);
    }

    // ── Additional: to_cep_policy_config backward compat ───────────

    #[test]
    fn policy_config_to_cep_policy_config() {
        let config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));
        let cep_context = config.to_cep_policy_config();
        assert_eq!(cep_context.project_root, config.project_root);
        assert_eq!(cep_context.observation, config.observation);
        assert_eq!(cep_context.budget(), config.observation.budget_limits());
    }

    // ── Additional: ExternalPolicyFile deserialization ─────────────────

    #[test]
    fn external_policy_file_deserializes_from_toml() {
        let toml_str = r#"
[methods."observe.workspace"]
enabled = true

[network]
allow_public_https = true
allowed_hosts = ["example.com"]

[budget]
max_single_read_bytes = 4096

[observation]
max_grep_matches = 500
"#;
        let external: ExternalPolicyFile = toml::from_str(toml_str).unwrap();
        assert!(external.methods.is_some());
        assert!(external.network.is_some());
        assert!(external.budget.is_some());
        assert!(external.observation.is_some());
    }
}
