// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default in-process policy hook implementation.
//!
//! Combines capability matching, command policy, network policy, and budget
//! enforcement into a single data-driven pure decision function.
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

use super::BudgetLimits;
use super::command::{CommandPolicy, EffectiveImpact};
use super::network::NetworkPolicyConfig;
use super::observation::ObservationPolicyConfig;
use super::resolved::PolicyResolver;
use lancet_protocol::effect_types::ImpactKind;

// ── Config-friendly source class ───────────────────────────────────────

/// Config-friendly source class for capability source policy.
///
/// This does not carry runtime envelope ids. A runtime
/// `CapabilitySource::Both { envelope_id }` satisfies both classes when
/// checking source policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilitySourceClass {
    Base,
    Declared,
}

// ── Capability source policy ───────────────────────────────────────────

/// Which source classes are allowed for each method family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySourcePolicy {
    pub observation: Vec<CapabilitySourceClass>,
    pub mutation: Vec<CapabilitySourceClass>,
    pub command: Vec<CapabilitySourceClass>,
    pub external: Vec<CapabilitySourceClass>,
    pub session_state: Vec<CapabilitySourceClass>,
}

impl Default for CapabilitySourcePolicy {
    fn default() -> Self {
        use CapabilitySourceClass::*;
        Self {
            observation: vec![Base, Declared],
            mutation: vec![Declared],
            command: vec![Declared],
            external: vec![Base, Declared],
            session_state: vec![Base, Declared],
        }
    }
}

// ── Method policy overrides ────────────────────────────────────────────

/// Fixed-field overrides for a single method.
///
/// Only fixed fields are allowed — this must not become a second policy engine.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MethodPolicy {
    pub enabled: Option<bool>,
    pub allowed_sources: Option<Vec<CapabilitySourceClass>>,
    pub required_capability: Option<lancet_catalog::method::RequiredCapability>,
    pub max_single_read_bytes: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

// ── Workspace write policy (blacklist) ─────────────────────────────────

/// Immutable workspace write-deny policy (blacklist).
///
/// A path is allowed unless it matches any denied pattern. Shared by
/// `mutate.*` and workspace-mutating `exec.command_run` decisions.
/// No system-level denies are hardcoded — the deny list is user-controlled.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspacePolicy {
    pub denied_write_patterns: Vec<String>,
}

impl WorkspacePolicy {
    /// Validate every configured deny pattern using Guard L1 write-pattern rules.
    pub fn validate(&self) -> Result<(), String> {
        use crate::guard::normalize_write_path_pattern;
        for pattern in &self.denied_write_patterns {
            if pattern.is_empty() {
                return Err("denied_write_patterns entry must not be empty".into());
            }
            if pattern.contains('\0') {
                return Err("denied_write_patterns entry must not contain NUL bytes".into());
            }
            if pattern.contains("..") {
                return Err("denied_write_patterns entry must not contain '..' traversal".into());
            }
            if Path::new(pattern).is_absolute() {
                return Err("denied_write_patterns entry must be project-root-relative".into());
            }
            normalize_write_path_pattern(pattern).map_err(|e| e.message)?;
        }
        Ok(())
    }

    /// Build the post-policy authorized region for a method decision.
    pub fn authorized_region_for(
        &self,
        method: &str,
        command_impact: Option<&EffectiveImpact>,
        policy: &super::CepPolicyConfig,
    ) -> crate::effect::region::AuthorizedEffectRegion {
        use crate::effect::region::AuthorizedEffectRegion;
        use lancet_protocol::effect_types::ImpactKind;

        let mut region = AuthorizedEffectRegion::empty();
        match method {
            "observe.workspace" => {
                region.kinds.insert(ImpactKind::WorkspaceObservation);
                // Observation effects are Upper-relative; authorize the workspace root sentinel.
                region.observation_roots = vec![PathBuf::from(".")];
            }
            "fetch.web" => {
                region.kinds.insert(ImpactKind::ExternalObservation);
                region.hosts = policy.network.allowed_hosts.clone();
            }
            "mutate.create" | "mutate.edit" | "mutate.batch" => {
                region.kinds.insert(ImpactKind::WorkspaceMutation);
                apply_denied_write_patterns(
                    &mut region,
                    &self.denied_write_patterns,
                    &policy.project_root,
                );
            }
            "exec.command_run" => {
                region.kinds.insert(ImpactKind::CommandExecution);
                region.command_allowed = true;
                if matches!(command_impact, Some(EffectiveImpact::WorkspaceMutation)) {
                    region.kinds.insert(ImpactKind::WorkspaceMutation);
                    apply_denied_write_patterns(
                        &mut region,
                        &self.denied_write_patterns,
                        &policy.project_root,
                    );
                }
            }
            "session.todo_write" => {
                region.kinds.insert(ImpactKind::SessionStateMutation);
                region.session_state_allowed = true;
            }
            "observe.audit" | "observe.audit_replay" => {
                region.kinds.insert(ImpactKind::AuditObservation);
                region.session_state_allowed = true;
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
    pub capability_sources: CapabilitySourcePolicy,
    pub methods: BTreeMap<String, MethodPolicy>,
    pub command: CommandPolicy,
    pub network: NetworkPolicyConfig,
    pub budget: BudgetLimits,
    pub observation: ObservationPolicyConfig,
    pub workspace: WorkspacePolicy,
    pub project_root: PathBuf,
    pub denied_effect_kinds: BTreeSet<ImpactKind>,
}

impl PolicyConfig {
    /// Compiled defaults for the given project root.
    pub fn defaults_for_project_root(project_root: PathBuf) -> Self {
        let observation = ObservationPolicyConfig::defaults_for_root(&project_root);
        let budget = BudgetLimits {
            max_single_read_bytes: observation.max_single_read_bytes,
            max_turn_observation_bytes: observation.max_turn_observation_bytes,
            max_grep_matches: observation.max_grep_matches,
            max_list_entries: observation.max_list_entries,
            max_find_paths: observation.max_find_paths,
            max_todo_ops: observation.max_todo_ops,
        };
        Self {
            capability_sources: CapabilitySourcePolicy::default(),
            methods: BTreeMap::new(),
            command: CommandPolicy::defaults(),
            network: NetworkPolicyConfig::safe_defaults(),
            budget,
            observation,
            workspace: WorkspacePolicy::default(),
            project_root,
            denied_effect_kinds: BTreeSet::new(),
        }
    }

    /// Merge an external TOML policy file into this config.
    pub fn merge_external(&mut self, external: &ExternalPolicyFile) -> Result<(), String> {
        if let Some(cap_src) = &external.capability_sources {
            if let Some(obs) = &cap_src.observation {
                self.capability_sources.observation = parse_source_classes(obs)?;
            }
            if let Some(mut_) = &cap_src.mutation {
                self.capability_sources.mutation = parse_source_classes(mut_)?;
            }
            if let Some(cmd) = &cap_src.command {
                self.capability_sources.command = parse_source_classes(cmd)?;
            }
            if let Some(ext) = &cap_src.external {
                self.capability_sources.external = parse_source_classes(ext)?;
            }
            if let Some(ss) = &cap_src.session_state {
                self.capability_sources.session_state = parse_source_classes(ss)?;
            }
        }
        if let Some(methods) = &external.methods {
            for (name, ext_method) in methods {
                let policy = MethodPolicy {
                    enabled: ext_method.enabled,
                    allowed_sources: ext_method
                        .allowed_sources
                        .as_ref()
                        .map(|v| parse_source_classes(v))
                        .transpose()?,
                    required_capability: ext_method
                        .required_capability
                        .as_ref()
                        .map(|s| parse_required_capability(s))
                        .transpose()?,
                    max_single_read_bytes: ext_method.max_single_read_bytes,
                    max_todo_ops: ext_method.max_todo_ops,
                };
                self.methods.insert(name.clone(), policy);
            }
        }
        if let Some(cmd) = &external.command {
            if let Some(v) = cmd.default_timeout_ms {
                self.command.default_timeout_ms = v;
            }
            if let Some(v) = cmd.max_timeout_ms {
                self.command.max_timeout_ms = v;
            }
            if let Some(v) = cmd.max_output_bytes {
                self.command.max_output_bytes = v;
            }
            if let Some(v) = cmd.allow_inline_shell {
                self.command.allow_inline_shell = v;
            }
            if let Some(v) = cmd.inline_shell_max_script_bytes {
                self.command.inline_shell_max_script_bytes = v;
            }
            if let Some(v) = cmd.inline_shell_max_commands {
                self.command.inline_shell_max_commands = v;
            }
            if let Some(v) = cmd.inline_shell_max_words_per_command {
                self.command.inline_shell_max_words_per_command = v;
            }
            if let Some(v) = &cmd.allowed_diagnostic_commands {
                self.command.allowed_diagnostic_commands = v.clone();
            }
            if let Some(v) = &cmd.denied_commands {
                self.command.denied_commands = v.clone();
            }
            if let Some(v) = &cmd.allowed_env {
                self.command.allowed_env = v.clone();
            }
            if let Some(v) = &cmd.inherited_env {
                self.command.inherited_env = v.clone();
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
        if let Some(budget) = &external.budget {
            if let Some(v) = budget.max_single_read_bytes {
                self.budget.max_single_read_bytes = v;
            }
            if let Some(v) = budget.max_turn_observation_bytes {
                self.budget.max_turn_observation_bytes = v;
            }
            if let Some(v) = budget.max_grep_matches {
                self.budget.max_grep_matches = v;
            }
            if let Some(v) = budget.max_list_entries {
                self.budget.max_list_entries = v;
            }
            if let Some(v) = budget.max_find_paths {
                self.budget.max_find_paths = v;
            }
            if let Some(v) = budget.max_todo_ops {
                self.budget.max_todo_ops = v;
            }
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
            self.workspace.validate()?;
        }
        if let Some(obs) = &external.observation {
            if let Some(v) = obs.max_single_read_bytes {
                self.observation.max_single_read_bytes = v;
            }
            if let Some(v) = obs.max_turn_observation_bytes {
                self.observation.max_turn_observation_bytes = v;
            }
            if let Some(v) = obs.max_grep_matches {
                self.observation.max_grep_matches = v;
            }
            if let Some(v) = obs.max_list_entries {
                self.observation.max_list_entries = v;
            }
            if let Some(v) = obs.max_find_paths {
                self.observation.max_find_paths = v;
            }
            if let Some(v) = obs.max_todo_ops {
                self.observation.max_todo_ops = v;
            }
        }
        Ok(())
    }

    /// Construct a `PolicyResolver` from this config.
    pub fn to_resolver(&self) -> PolicyResolver {
        PolicyResolver {
            project_root: self.project_root.clone(),
            observation: self.observation.clone(),
            network: self.network.clone(),
            command: self.command.clone(),
            budget: self.budget.clone(),
            workspace: self.workspace.clone(),
        }
    }

    /// Construct a `CepPolicyConfig` for backward compatibility with
    /// existing CEP handlers during the wrapper-first migration.
    pub fn to_cep_policy_config(&self) -> super::CepPolicyConfig {
        super::CepPolicyConfig {
            project_root: self.project_root.clone(),
            observation: self.observation.clone(),
            network: self.network.clone(),
            budget: self.budget.clone(),
            command: self.command.clone(),
            denied_effect_kinds: self.denied_effect_kinds.clone(),
        }
    }
}

// ── External TOML policy file DTOs ─────────────────────────────────────

/// External TOML policy file structure.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPolicyFile {
    pub capability_sources: Option<ExternalCapabilitySourcePolicy>,
    pub methods: Option<BTreeMap<String, ExternalMethodPolicy>>,
    pub command: Option<ExternalCommandPolicy>,
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
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalCapabilitySourcePolicy {
    pub observation: Option<Vec<String>>,
    pub mutation: Option<Vec<String>>,
    pub command: Option<Vec<String>>,
    pub external: Option<Vec<String>>,
    pub session_state: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalMethodPolicy {
    pub enabled: Option<bool>,
    pub allowed_sources: Option<Vec<String>>,
    pub required_capability: Option<String>,
    pub max_single_read_bytes: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalCommandPolicy {
    pub default_timeout_ms: Option<u64>,
    pub max_timeout_ms: Option<u64>,
    pub max_output_bytes: Option<usize>,
    pub allow_inline_shell: Option<bool>,
    pub inline_shell_max_script_bytes: Option<usize>,
    pub inline_shell_max_commands: Option<usize>,
    pub inline_shell_max_words_per_command: Option<usize>,
    pub allowed_diagnostic_commands: Option<Vec<Vec<String>>>,
    pub denied_commands: Option<Vec<Vec<String>>>,
    pub allowed_env: Option<Vec<String>>,
    pub inherited_env: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalNetworkPolicy {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    pub allowed_hosts: Option<Vec<String>>,
    pub max_web_response_bytes: Option<usize>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalBudgetPolicy {
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

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

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEffectsPolicy {
    #[serde(default)]
    pub deny: Option<Vec<ImpactKind>>,
}

fn parse_source_classes(strings: &[String]) -> Result<Vec<CapabilitySourceClass>, String> {
    let mut result = Vec::new();
    for s in strings {
        match s.as_str() {
            "base" => result.push(CapabilitySourceClass::Base),
            "declared" => result.push(CapabilitySourceClass::Declared),
            other => {
                return Err(format!(
                    "unknown source class '{}'; expected 'base' or 'declared'",
                    other
                ));
            }
        }
    }
    Ok(result)
}

fn parse_required_capability(
    s: &str,
) -> Result<lancet_catalog::method::RequiredCapability, String> {
    use lancet_catalog::method::RequiredCapability;
    match s {
        "Inspect" => Ok(RequiredCapability::Inspect),
        "ReadPath" => Ok(RequiredCapability::ReadPath),
        "WritePath" => Ok(RequiredCapability::WritePath),
        "RunCommand" => Ok(RequiredCapability::RunCommand),
        "FetchUrl" => Ok(RequiredCapability::FetchUrl),
        "MutateSessionState" => Ok(RequiredCapability::MutateSessionState),
        "ReadAudit" => Ok(RequiredCapability::ReadAudit),
        other => Err(format!(
            "unknown required_capability '{}'; expected Inspect, ReadPath, WritePath, RunCommand, FetchUrl, MutateSessionState, or ReadAudit",
            other
        )),
    }
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
    pub command_classification: Option<EffectiveImpact>,
    pub network_host: Option<String>,
    pub budget_remaining: Option<u64>,
    pub resolved_paths: Vec<String>,
    /// Sub-command argvs of a validated inline-shell expansion, when the
    /// invocation was an `exec.command_run` shell carrier.
    pub shell_expansion: Option<Vec<Vec<String>>>,
}

// ── Default policy hook ────────────────────────────────────────────────

/// Default in-process policy hook.
///
/// Combines capability matching, command policy, network policy, and budget
/// enforcement into a single data-driven pure decision function.
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
    /// Direct mode: enabled-method → command → network → budget → workspace write region.
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

        if resolved.method == "exec.command_run" {
            if let Some(cmd_err) = &resolved.command_error {
                return RichPolicyOutcome {
                    decision: PolicyDecision::Denied {
                        reason_code: PolicyReasonCode::CommandDenied,
                        message: cmd_err.message.clone(),
                        action_required: Some(PolicyActionRequired::ChangeCommand),
                    },
                    evidence: PolicyEvidence {
                        command_classification: resolved
                            .resolved_command
                            .as_ref()
                            .map(|c| c.effective_impact.clone()),
                        ..Default::default()
                    },
                };
            }
            if let Some(cmd) = &resolved.resolved_command
                && cmd.effective_impact == EffectiveImpact::HardDenied
            {
                return RichPolicyOutcome {
                    decision: PolicyDecision::Denied {
                        reason_code: PolicyReasonCode::CommandDenied,
                        message: format!("command denied by policy: {:?}", cmd.argv),
                        action_required: Some(PolicyActionRequired::ChangeCommand),
                    },
                    evidence: PolicyEvidence {
                        command_classification: Some(cmd.effective_impact.clone()),
                        ..Default::default()
                    },
                };
            }
            if let Some(cmd) = &resolved.resolved_command
                && cmd.effective_impact == EffectiveImpact::WorkspaceMutation
            {
                // Blacklist semantics: an empty denied_write_patterns means all
                // workspace paths are allowed. No deny-at-policy-time here;
                // per-path deny enforcement happens in Guard L1 / validate_path_facts.
                let _ = cmd;
            }
        }

        // No allowlist gate for mutate.* — blacklist semantics mean an empty
        // denied_write_patterns allows all workspace paths. Per-path deny
        // enforcement happens in Guard L1 / validate_path_facts.

        if let Some(denied) = self.check_network_and_budget(resolved, method_policy) {
            return denied;
        }

        self.allowed_outcome_direct(resolved)
    }

    fn check_network_and_budget(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
        method_policy: Option<&MethodPolicy>,
    ) -> Option<RichPolicyOutcome> {
        if resolved.method == "fetch.web"
            && let Some(net) = &resolved.resolved_network
            && !self.config.network.allowed_hosts.is_empty()
        {
            let host_lower = net.host.to_ascii_lowercase();
            let allowed = self.config.network.allowed_hosts.iter().any(|h| {
                let h = h.trim().to_ascii_lowercase();
                if h.starts_with('.') {
                    let suffix = h.trim_start_matches('.').to_ascii_lowercase();
                    host_lower == suffix || host_lower.ends_with(&format!(".{suffix}"))
                } else {
                    host_lower == h
                }
            });
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
        let effective_todo_limit = method_policy
            .and_then(|mp| mp.max_todo_ops)
            .map(|mp_limit| (mp_limit as u64).min(budget.todo_ops_limit))
            .unwrap_or(budget.todo_ops_limit);
        if resolved.method == "session.todo_write" && budget.todo_ops_used >= effective_todo_limit {
            return Some(RichPolicyOutcome {
                decision: PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::BudgetExceeded,
                    message: "todo ops budget exhausted".into(),
                    action_required: Some(PolicyActionRequired::ReduceReadSize),
                },
                evidence: PolicyEvidence {
                    budget_remaining: Some(
                        effective_todo_limit.saturating_sub(budget.todo_ops_used),
                    ),
                    ..Default::default()
                },
            });
        }
        None
    }

    fn allowed_outcome_direct(
        &self,
        resolved: &super::resolved::ResolvedPolicyInput,
    ) -> RichPolicyOutcome {
        let budget = &resolved.budget_snapshot;
        let mut resolved_paths = Vec::new();
        if !resolved.invocation_facts.all_patch_targets.is_empty() {
            resolved_paths.extend(resolved.invocation_facts.all_patch_targets.iter().cloned());
        } else if let Some(target) = &resolved.resolved_target {
            resolved_paths.push(target.request_path.clone());
        }
        RichPolicyOutcome {
            decision: PolicyDecision::Allowed,
            evidence: PolicyEvidence {
                command_classification: resolved
                    .resolved_command
                    .as_ref()
                    .map(|c| c.effective_impact.clone()),
                network_host: resolved.resolved_network.as_ref().map(|n| n.host.clone()),
                budget_remaining: Some(
                    budget
                        .turn_observation_bytes_limit
                        .saturating_sub(budget.turn_observation_bytes_used),
                ),
                resolved_paths,
                shell_expansion: resolved
                    .resolved_command
                    .as_ref()
                    .filter(|c| !c.sub_argvs.is_empty())
                    .map(|c| c.sub_argvs.clone()),
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
    use super::*;
    use lancet_protocol::policy::{
        PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot,
    };
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

    // ── Test 1: observe.workspace read_file allowed in direct mode ──────

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

    // ── Test 3: exec.command_run with denied argv ───────────────────────────

    #[test]
    fn default_hook_denies_command_run_with_denied_argv() {
        let hook = DefaultPolicyHook::new(test_config());
        let input = PolicyInput {
            method: "exec.command_run".into(),
            params: json!({
                "argv": ["sh", "-c", "rm -rf /"],
                "declared_impact": "workspace_mutation"
            }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(default_budget()),
        };
        let outcome = hook.decide_with_evidence(&input);
        assert!(matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::CommandDenied,
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
        assert!(!config.command.allowed_diagnostic_commands.is_empty());
        assert!(!config.command.denied_commands.is_empty());
        assert!(config.network.allow_public_https);
        assert!(config.budget.max_single_read_bytes > 0);
        assert!(config.observation.max_single_read_bytes > 0);
        assert_eq!(
            config.capability_sources.observation,
            vec![CapabilitySourceClass::Base, CapabilitySourceClass::Declared]
        );
    }

    // ── Test 8: PolicyConfig merge precedence ──────────────────────────

    #[test]
    fn policy_config_merge_precedence_external_overrides_defaults() {
        let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/tmp/test"));
        let original_timeout = config.command.default_timeout_ms;

        let toml_str = r#"
[command]
default_timeout_ms = 99999

[network]
allow_public_https = false
"#;
        let external: ExternalPolicyFile = toml::from_str(toml_str).unwrap();
        config.merge_external(&external).unwrap();

        assert_eq!(config.command.default_timeout_ms, 99999);
        assert_ne!(config.command.default_timeout_ms, original_timeout);
        assert!(!config.network.allow_public_https);
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
        assert_eq!(
            cep_context.budget.max_single_read_bytes,
            config.budget.max_single_read_bytes
        );
    }

    // ── Additional: ExternalPolicyFile deserialization ─────────────────

    #[test]
    fn external_policy_file_deserializes_from_toml() {
        let toml_str = r#"
[capability_sources]
observation = ["base", "declared"]
mutation = ["declared"]

[methods."observe.workspace"]
enabled = true

[command]
default_timeout_ms = 60000

[network]
allow_public_https = true
allowed_hosts = ["example.com"]

[budget]
max_single_read_bytes = 4096

[observation]
max_grep_matches = 500
"#;
        let external: ExternalPolicyFile = toml::from_str(toml_str).unwrap();
        assert!(external.capability_sources.is_some());
        assert!(external.methods.is_some());
        assert!(external.command.is_some());
        assert!(external.network.is_some());
        assert!(external.budget.is_some());
        assert!(external.observation.is_some());
    }
}
