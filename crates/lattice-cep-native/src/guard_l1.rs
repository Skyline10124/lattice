//! Guard L1 path scope enforcement for workspace mutations.
//!
//! Guard L1 checks whether a target path falls within the active envelope's
//! `WritePath` capability scope. It distinguishes between:
//! - `ReplanRequired`: target is inside project root but outside active scope
//! - `GuardDenied`: target escapes project root or violates hard security boundary
//! - `EnvelopeMissing`: no active envelope authority exists

use crate::envelope_store::EnvelopeRecord;
use crate::policy::default::{DefaultPolicyHook, PolicyEvidence};
use crate::registry::HandlerContext;
use lattice_cep_protocol::envelope::{BaseExecutionEnvelope, Capability, GuardVerdict};
use lattice_cep_protocol::error::{
    CepErrorData, GuardErrorData, PermissionErrorData, ReservedErrorData, ValidationErrorData,
};
use lattice_cep_protocol::policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyAuditContext, PolicyBudgetSnapshot,
    PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode, SourcedCapability,
};
use lattice_cep_protocol::{CepError, CepErrorCode, Retriability};
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

/// Normalized write path pattern for scope matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WritePathPattern {
    /// Matches a single file exactly (e.g., "src/main.rs").
    ExactFile(PathBuf),
    /// Matches all files under a directory (e.g., "src/**" -> base "src").
    DirectoryGlob(PathBuf),
}

/// Normalize a WritePath pattern string into a typed pattern.
///
/// Supported forms:
/// - Exact file: "src/main.rs" -> `ExactFile("src/main.rs")`
/// - Directory glob: "src/**" -> `DirectoryGlob("src")`
/// - Simple prefix: "src" -> `DirectoryGlob("src")` (normalized to dir/**)
///
/// Rejected:
/// - Patterns containing ".." -> Validation error
/// - Patterns containing NUL -> Validation error
/// - Empty patterns -> Validation error
/// - Absolute paths as pattern syntax -> Validation error
pub fn normalize_write_path_pattern(pattern: &str) -> Result<WritePathPattern, CepError> {
    // Reject empty
    if pattern.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "WritePath pattern must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject NUL bytes
    if pattern.contains('\0') {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern contains NUL byte",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "contains NUL byte".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject ".." traversal
    if pattern.contains("..") {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern contains '..' traversal",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "contains '..' traversal".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject absolute paths as pattern syntax
    if Path::new(pattern).is_absolute() {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern must be project-root-relative, not absolute",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "absolute path not allowed as pattern".into(),
            }),
            Retriability::Permanent,
        ));
    }

    if pattern.starts_with('!') {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "WritePath pattern negation is not supported in CEP",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "negation is not supported".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Check for directory glob suffix FIRST, then normalize the base.
    if let Some(base) = pattern.strip_suffix("/**") {
        if base.is_empty() {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                "WritePath pattern '/**' is not valid; use a directory name before /**",
                CepErrorData::Validation(ValidationErrorData {
                    field: "capabilities.WritePath.path_pattern".into(),
                    reason: "bare '/**' not valid".into(),
                }),
                Retriability::Permanent,
            ));
        }
        let normalized = normalize_lexical(base)?;
        Ok(WritePathPattern::DirectoryGlob(normalized))
    } else {
        if pattern.contains('*')
            || pattern.contains('?')
            || pattern.contains('[')
            || pattern.contains(']')
            || pattern.contains('{')
            || pattern.contains('}')
        {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                "WritePath pattern uses unsupported glob syntax",
                CepErrorData::Validation(ValidationErrorData {
                    field: "capabilities.WritePath.path_pattern".into(),
                    reason: "unsupported glob syntax".into(),
                }),
                Retriability::Permanent,
            ));
        }

        if let Some(base) = pattern.strip_suffix('/') {
            if base.is_empty() {
                return Err(CepError::new(
                    CepErrorCode::SchemaMismatch,
                    "WritePath directory prefix must not be empty",
                    CepErrorData::Validation(ValidationErrorData {
                        field: "capabilities.WritePath.path_pattern".into(),
                        reason: "empty directory prefix".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            return Ok(WritePathPattern::DirectoryGlob(normalize_lexical(base)?));
        }

        // Normalize lexical path (resolve ".", reject "..")
        let normalized = normalize_lexical(pattern)?;

        // Determine if this looks like a directory or a file.
        // Simple single-component names without extensions -> DirectoryGlob.
        // Otherwise -> ExactFile.
        let path = Path::new(pattern);
        let is_single_component = !pattern.contains('/');
        let has_no_extension = path.extension().is_none();
        if is_single_component && has_no_extension {
            // Simple directory name like "src" -> DirectoryGlob
            Ok(WritePathPattern::DirectoryGlob(normalized))
        } else {
            Ok(WritePathPattern::ExactFile(normalized))
        }
    }
}

/// Normalize a path lexically: resolve `.` components, reject `..` and NUL.
fn normalize_lexical(path: &str) -> Result<PathBuf, CepError> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {} // skip .
            Component::ParentDir => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path contains '..' traversal: '{path}'"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "path".into(),
                        reason: "contains '..'".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path must be relative: '{path}'"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "path".into(),
                        reason: "absolute path not allowed".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }
    Ok(result)
}

/// Extract all WritePath path_patterns from a capability set.
pub fn extract_write_path_patterns(capabilities: &BTreeSet<Capability>) -> Vec<String> {
    capabilities
        .iter()
        .filter_map(|cap| match cap {
            Capability::WritePath { path_pattern } => Some(path_pattern.clone()),
            _ => None,
        })
        .collect()
}

/// Check if any RunCommand capability's argv_allowlist matches the given argv (prefix match).
pub fn has_run_command_capability(capabilities: &BTreeSet<Capability>, argv: &[String]) -> bool {
    for cap in capabilities {
        if let Capability::RunCommand { argv_allowlist } = cap {
            for entry in argv_allowlist {
                let tokens: Vec<&str> = entry.split_whitespace().collect();
                if argv.len() >= tokens.len()
                    && argv
                        .iter()
                        .zip(tokens.iter())
                        .all(|(a, t)| a.as_str() == *t)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Check whether a target path is within the WritePath scope of the active envelope.
///
/// Returns `Ok(())` if the target matches any WritePath pattern.
/// Returns `Err(ReplanRequired)` if target is inside project root but outside scope.
/// Returns `Err(GuardDenied)` if target escapes project root or symlink escape detected.
///
/// All paths are resolved relative to `project_root`.
///
/// For new file creation, walks up ancestors until finding an existing directory,
/// canonicalizes that directory, and checks containment.
pub fn check_write_scope(
    target_path: &str,
    project_root: &str,
    write_path_patterns: &[String],
) -> Result<(), CepError> {
    // Reject NUL in target
    if target_path.contains('\0') {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "target path contains NUL byte",
            CepErrorData::Guard(GuardErrorData {
                method: "guard.l1".into(),
                reason: "target path contains NUL byte".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject absolute target paths
    if Path::new(target_path).is_absolute() {
        return Err(guard_denied("target path must be project-root-relative"));
    }

    // Check for '..' in target — this is a GuardDenied (escape attempt), not validation
    if target_path.contains("..") {
        return Err(guard_denied(&format!(
            "target path contains '..' traversal: '{target_path}'"
        )));
    }

    // Lexically normalize target
    let target_normalized = normalize_lexical(target_path)?;

    // Canonicalize project root
    let project_root_canonical = std::fs::canonicalize(project_root).map_err(|e| {
        guard_denied(&format!(
            "cannot canonicalize project root '{}': {e}",
            project_root
        ))
    })?;

    // Resolve target as project_root / target_normalized
    let target_resolved = project_root_canonical.join(&target_normalized);

    // Case A or B: canonicalize target
    let target_canonical = canonicalize_target(&target_resolved, &project_root_canonical)?;

    // Check target is inside project root
    if target_canonical != project_root_canonical
        && !target_canonical.starts_with(&project_root_canonical)
    {
        return Err(guard_denied(&format!(
            "target '{}' escapes project root '{}'",
            target_canonical.display(),
            project_root_canonical.display()
        )));
    }

    // Check each WritePath pattern
    for pattern_str in write_path_patterns {
        let pattern = normalize_write_path_pattern(pattern_str)?;
        match pattern {
            WritePathPattern::ExactFile(pattern_path) => {
                let pattern_resolved = project_root_canonical.join(&pattern_path);
                let pattern_canonical =
                    canonicalize_pattern_path(&pattern_resolved, &project_root_canonical)?;
                if target_canonical == pattern_canonical {
                    return Ok(());
                }
            }
            WritePathPattern::DirectoryGlob(dir_path) => {
                let dir_resolved = project_root_canonical.join(&dir_path);
                let dir_canonical =
                    canonicalize_pattern_dir(&dir_resolved, &project_root_canonical)?;
                if target_canonical.starts_with(&dir_canonical) {
                    return Ok(());
                }
            }
        }
    }

    // No pattern matched — target is inside project root but outside scope
    Err(replan_required(target_path, write_path_patterns))
}

/// Canonicalize a target path, handling both existing and non-existing files.
///
/// Case A: file exists -> canonicalize directly, check for symlinks.
/// Case B: file doesn't exist -> walk up to find existing ancestor, canonicalize that,
///         then construct canonical path from ancestor + remaining relative path.
fn canonicalize_target(target: &Path, project_root_canonical: &Path) -> Result<PathBuf, CepError> {
    // Try direct canonicalization (Case A: file exists)
    if let Ok(canonical) = std::fs::canonicalize(target) {
        // Check for symlink at target itself
        if let Ok(meta) = std::fs::symlink_metadata(target) {
            if meta.file_type().is_symlink() {
                return Err(permission_denied(&format!(
                    "target '{}' is a symlink",
                    target.display()
                )));
            }
        }
        return Ok(canonical);
    }

    // Case B: file doesn't exist — walk up ancestors
    let mut current = target.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            // Found an existing ancestor
            // Check if this ancestor is inside project root
            if canonical != *project_root_canonical
                && !canonical.starts_with(project_root_canonical)
            {
                return Err(guard_denied(&format!(
                    "ancestor '{}' of target '{}' escapes project root",
                    canonical.display(),
                    target.display()
                )));
            }
            // Construct canonical target from canonical ancestor + remaining path
            if let Ok(relative) = target.strip_prefix(&current) {
                return Ok(canonical.join(relative));
            } else {
                return Ok(canonical);
            }
        }
        // Try parent
        if !current.pop() {
            return Err(guard_denied(&format!(
                "cannot resolve any ancestor of target '{}'",
                target.display()
            )));
        }
    }
}

/// Canonicalize a pattern path for exact file matching.
fn canonicalize_pattern_path(
    pattern: &Path,
    project_root_canonical: &Path,
) -> Result<PathBuf, CepError> {
    // If file exists, canonicalize it
    if let Ok(canonical) = std::fs::canonicalize(pattern) {
        return Ok(canonical);
    }
    // File doesn't exist — canonicalize parent + filename
    if let Some(parent) = pattern.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(project_root_canonical.join(pattern));
        }
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            if let Some(filename) = pattern.file_name() {
                return Ok(canonical_parent.join(filename));
            }
        }
    }
    Ok(project_root_canonical.join(
        pattern
            .strip_prefix(project_root_canonical)
            .unwrap_or(pattern),
    ))
}

/// Canonicalize a pattern directory for glob matching.
fn canonicalize_pattern_dir(
    dir: &Path,
    _project_root_canonical: &Path,
) -> Result<PathBuf, CepError> {
    if let Ok(canonical) = std::fs::canonicalize(dir) {
        // Check for symlink in pattern base
        if let Ok(meta) = std::fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() {
                return Err(permission_denied(&format!(
                    "WritePath pattern base '{}' is a symlink pointing outside root",
                    dir.display()
                )));
            }
        }
        return Ok(canonical);
    }
    // Directory doesn't exist yet — use resolved path
    Ok(dir.to_path_buf())
}

fn guard_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: reason.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn permission_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: reason.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn replan_required(target: &str, patterns: &[String]) -> CepError {
    CepError::new(
        CepErrorCode::ReplanRequired,
        format!(
            "target '{}' is not within active WritePath scope: {:?}",
            target, patterns
        ),
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: format!(
                "ReplanRequired: target '{}' outside scope {:?}",
                target, patterns
            ),
        }),
        Retriability::RequiresReplan,
    )
}

/// Build a layered effective envelope snapshot from base + declared envelopes.
///
/// The base envelope is the turn-level observation floor; a declared envelope
/// is the task-level authority extension. Capabilities present in both get
/// `Both` source.
pub fn build_effective_envelope_snapshot(
    base: &BaseExecutionEnvelope,
    active: Option<&EnvelopeRecord>,
) -> EffectiveEnvelopeSnapshot {
    let base_envelope_id = "base".to_string();
    let declared_envelope_id = active.map(|r| r.envelope_id.clone());
    let base_caps: &BTreeSet<Capability> = &base.inner.capabilities;
    let declared_caps: Option<&BTreeSet<Capability>> = active.map(|r| &r.envelope.capabilities);

    let mut capabilities = Vec::new();
    let mut matched_declared: HashSet<usize> = HashSet::new();

    for base_cap in base_caps {
        let source = if let Some(d_caps) = declared_caps {
            let matching_idx = d_caps.iter().enumerate().find_map(|(idx, d_cap)| {
                if d_cap == base_cap && !matched_declared.contains(&idx) {
                    Some(idx)
                } else {
                    None
                }
            });
            match matching_idx {
                Some(idx) => {
                    matched_declared.insert(idx);
                    CapabilitySource::Both {
                        envelope_id: declared_envelope_id.clone().unwrap_or_default(),
                    }
                }
                None => CapabilitySource::Base,
            }
        } else {
            CapabilitySource::Base
        };
        capabilities.push(SourcedCapability {
            capability: base_cap.clone(),
            source,
        });
    }

    if let Some(d_caps) = declared_caps {
        for (idx, d_cap) in d_caps.iter().enumerate() {
            if !matched_declared.contains(&idx) {
                capabilities.push(SourcedCapability {
                    capability: d_cap.clone(),
                    source: CapabilitySource::Declared {
                        envelope_id: declared_envelope_id.clone().unwrap_or_default(),
                    },
                });
            }
        }
    }

    EffectiveEnvelopeSnapshot {
        base_envelope_id,
        declared_envelope_id,
        capabilities,
    }
}

/// Build a `PolicyInput` from handler context, method name, and params.
///
/// Assembles the effective envelope snapshot, audit context (including budget snapshot),
/// and serialized params into the public policy input contract.
pub async fn build_policy_input(
    ctx: &HandlerContext,
    method: &str,
    params: &impl Serialize,
) -> Result<PolicyInput, CepError> {
    let active_record = ctx.active_envelope.active_record().await;
    let envelope_snapshot =
        build_effective_envelope_snapshot(&ctx.base_envelope, active_record.as_ref());

    let audit_snapshot = ctx.audit_context.read().await;
    let budget = ctx.cep_context.budget.snapshot();
    let budget_snapshot = PolicyBudgetSnapshot {
        turn_observation_bytes_used: budget.turn_observation_bytes_used as u64,
        turn_observation_bytes_limit: budget.max_turn_observation_bytes as u64,
        todo_ops_used: budget.todo_ops_used as u64,
        todo_ops_limit: budget.max_todo_ops as u64,
    };

    let audit_context = PolicyAuditContext {
        turn_id: audit_snapshot.turn_id.clone(),
        session_id: audit_snapshot.session_id.clone(),
        call_id: Some(ctx.call_id),
        budget_snapshot,
    };

    Ok(PolicyInput {
        method: method.to_string(),
        params: serde_json::to_value(params).map_err(|e| {
            CepError::internal_error(format!("failed to serialize policy input params: {e}"))
        })?,
        envelope_snapshot,
        audit_context,
    })
}

/// Result of evaluating a policy hook.
pub struct PolicyEvaluation {
    pub decision: PolicyDecision,
    pub evidence: Option<PolicyEvidence>,
}

/// Evaluate the policy hook, returning a stable decision and optional evidence.
///
/// If the default hook is installed, calls `decide_with_evidence` to capture
/// rich executor-owned evidence for audit. Otherwise calls the public
/// `PolicyHook::decide` and returns `evidence: None`.
pub fn evaluate_policy(
    hook: &dyn PolicyHook,
    default_hook: Option<&DefaultPolicyHook>,
    input: &PolicyInput,
) -> Result<PolicyEvaluation, CepError> {
    if let Some(default) = default_hook {
        let outcome = default.decide_with_evidence(input);
        Ok(PolicyEvaluation {
            decision: outcome.decision,
            evidence: Some(outcome.evidence),
        })
    } else {
        let decision = hook.decide(input);
        Ok(PolicyEvaluation {
            decision,
            evidence: None,
        })
    }
}

/// Map a `PolicyDecision` to a CEP error if denied/human-review, or `Ok(())` if allowed.
///
/// Every `PolicyReasonCode` maps to an existing `CepErrorCode`. This is the single
/// mapping path: PolicyDecision → CEP typed error → event → audit.
pub fn map_decision_to_cep_error(decision: &PolicyDecision, method: &str) -> Result<(), CepError> {
    match decision {
        PolicyDecision::Allowed => Ok(()),
        PolicyDecision::Denied {
            reason_code,
            message,
            action_required: _,
        } => {
            let (code, data, retriability) = match reason_code {
                PolicyReasonCode::MissingCapability | PolicyReasonCode::EnvelopeRequired => (
                    CepErrorCode::CapabilityNotGranted,
                    CepErrorData::Permission(PermissionErrorData {
                        required_capability: method.into(),
                        active_envelope_id: None,
                        granted_capabilities: vec![],
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::CapabilitySourceNotAllowed => (
                    CepErrorCode::CapabilityNotGranted,
                    CepErrorData::Permission(PermissionErrorData {
                        required_capability: method.into(),
                        active_envelope_id: None,
                        granted_capabilities: vec![],
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::PathOutsideScope | PolicyReasonCode::ReplanRequired => (
                    CepErrorCode::ReplanRequired,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::RequiresReplan,
                ),
                PolicyReasonCode::CommandDenied | PolicyReasonCode::NetworkDenied => (
                    CepErrorCode::GuardDenied,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::BudgetExceeded => (
                    CepErrorCode::Reserved,
                    CepErrorData::Reserved(ReservedErrorData {
                        code: "budget_exceeded".into(),
                        detail: message.clone(),
                    }),
                    Retriability::RequiresReplan,
                ),
                PolicyReasonCode::PolicyConfigInvalid => (
                    CepErrorCode::Reserved,
                    CepErrorData::Reserved(ReservedErrorData {
                        code: "policy_config_invalid".into(),
                        detail: message.clone(),
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::NonDeterminable => (
                    CepErrorCode::GuardDenied,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::Unknown,
                ),
            };
            Err(CepError::new(code, message.clone(), data, retriability))
        }
        PolicyDecision::HumanReview {
            reason_code: _,
            message,
        } => Err(CepError::new(
            CepErrorCode::GuardDenied,
            message.clone(),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: message.clone(),
            }),
            Retriability::Unknown,
        )),
    }
}

/// Map a `PolicyDecision` to a `GuardVerdict` for event compatibility.
///
/// This is a projection for event emission only — `PolicyDecision` and
/// `GuardVerdict` remain distinct types.
pub fn decision_to_guard_verdict(decision: &PolicyDecision) -> GuardVerdict {
    match decision {
        PolicyDecision::Allowed => GuardVerdict::Allowed,
        PolicyDecision::Denied { message, .. } => GuardVerdict::Denied {
            reason: message.clone(),
        },
        PolicyDecision::HumanReview { message, .. } => GuardVerdict::HumanReview {
            reason: message.clone(),
        },
    }
}

/// Evaluate policy, emit guard verdict event, record metadata, and enforce decision.
///
/// This is the single entry point for handlers to route through the policy system.
/// It handles:
/// 1. Policy evaluation via `evaluate_policy`
/// 2. Guard verdict event emission
/// 3. Evidence metadata recording on success
/// 4. Denial metadata recording on failure
/// 5. CEP error mapping via `map_decision_to_cep_error`
pub async fn evaluate_and_enforce_policy(
    ctx: &HandlerContext,
    method: &str,
    params: &impl Serialize,
) -> Result<PolicyEvaluation, CepError> {
    let input = build_policy_input(ctx, method, params).await?;
    let evaluation = evaluate_policy(
        &*ctx.policy_hook,
        ctx.default_policy_hook.as_deref(),
        &input,
    )?;
    let verdict = decision_to_guard_verdict(&evaluation.decision);
    ctx.events
        .emit(
            "cep.guard_verdict",
            serde_json::json!({ "method": method, "verdict": verdict }),
        )
        .await;

    match &evaluation.decision {
        PolicyDecision::Allowed => {
            if let Some(evidence) = &evaluation.evidence {
                ctx.record_durable_metadata(serde_json::json!({
                    "cep_policy": {
                        "capability_match": evidence.capability_match.is_some(),
                        "budget_remaining": evidence.budget_remaining,
                    }
                }));

                let matched_cap = evidence
                    .capability_match
                    .as_ref()
                    .and_then(|cm| cm.matched.as_ref());
                let region = crate::effect::region::AuthorizedEffectRegion::from_matched_capability(
                    matched_cap,
                    &ctx.cep_context.policy,
                );
                ctx.authorized_region.set(region);
            }
        }
        PolicyDecision::Denied { reason_code, .. }
        | PolicyDecision::HumanReview { reason_code, .. } => {
            ctx.record_durable_metadata(serde_json::json!({
                "method": method,
                "denial_source": "cep_policy",
                "policy_enforced": true,
                "backend_invoked": false,
                "reason_code": format!("{:?}", reason_code),
            }));
        }
    }

    map_decision_to_cep_error(&evaluation.decision, method)?;
    Ok(evaluation)
}

#[cfg(test)]
mod guard_l1_tests {
    use super::*;
    use tempfile::TempDir;

    // --- normalize_write_path_pattern tests ---

    #[test]
    fn exact_file_pattern() {
        let pattern = normalize_write_path_pattern("src/main.rs").unwrap();
        assert!(matches!(pattern, WritePathPattern::ExactFile(_)));
    }

    #[test]
    fn dir_glob_pattern() {
        let pattern = normalize_write_path_pattern("src/**").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src"))
        );
    }

    #[test]
    fn simple_prefix_normalized_to_dir_glob() {
        let pattern = normalize_write_path_pattern("src").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src"))
        );
    }

    #[test]
    fn reject_dot_dot() {
        let err = normalize_write_path_pattern("../etc/passwd").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn reject_nul() {
        let err = normalize_write_path_pattern("src/\0file").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn reject_empty() {
        let err = normalize_write_path_pattern("").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn reject_absolute_pattern() {
        let err = normalize_write_path_pattern("/etc/passwd").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn nested_dir_glob() {
        let pattern = normalize_write_path_pattern("src/lib/**").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src/lib"))
        );
    }

    #[test]
    fn trailing_slash_directory_prefix_normalized_to_dir_glob() {
        let pattern = normalize_write_path_pattern("crates/lattice-cep-native/").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("crates/lattice-cep-native"))
        );
    }

    #[test]
    fn reject_unsupported_glob_syntax() {
        let err = normalize_write_path_pattern("**/*.rs").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn reject_negation_syntax() {
        let err = normalize_write_path_pattern("!src/**").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    // --- check_write_scope tests ---

    #[test]
    fn exact_file_match() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let result = check_write_scope("main.rs", root, &["main.rs".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn dir_glob_matches_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src/lib")).unwrap();
        std::fs::write(dir.path().join("src/lib/mod.rs"), "pub mod foo;").unwrap();

        let result = check_write_scope("src/lib/mod.rs", root, &["src/**".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn simple_prefix_matches_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let result = check_write_scope("src/main.rs", root, &["src".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn target_outside_scope_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();

        let err = check_write_scope("README.md", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn target_outside_project_root_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("../outside.txt", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn pattern_with_dot_dot_rejected() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("src/main.rs", root, &["../etc/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn nul_in_target_rejected() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("src/\0file", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "outside").unwrap();

        // Create a symlink inside project root pointing outside
        std::os::unix::fs::symlink(&outside_file, dir.path().join("link.txt")).unwrap();

        let err = check_write_scope("link.txt", root, &["link.txt".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn multiple_patterns_first_match_wins() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let patterns = vec!["tests/**".into(), "src/**".into()];
        let result = check_write_scope("src/main.rs", root, &patterns);
        assert!(result.is_ok());
    }

    #[test]
    fn no_patterns_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("file.txt"), "data").unwrap();

        let err = check_write_scope("file.txt", root, &[]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[test]
    fn new_file_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        let result = check_write_scope("src/new_file.rs", root, &["src/**".into()]);
        assert!(result.is_ok());
    }

    // --- extract_write_path_patterns tests ---

    #[test]
    fn extracts_write_path_patterns() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        caps.insert(Capability::ReadPath {
            path_pattern: ".".into(),
        });
        caps.insert(Capability::WritePath {
            path_pattern: "tests/**".into(),
        });

        let patterns = extract_write_path_patterns(&caps);
        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"src/**".to_string()));
        assert!(patterns.contains(&"tests/**".to_string()));
    }

    // --- has_run_command_capability tests ---

    #[test]
    fn has_run_command_prefix_match() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::RunCommand {
            argv_allowlist: vec!["cargo test".into(), "cargo check".into()],
        });

        assert!(has_run_command_capability(
            &caps,
            &["cargo".into(), "test".into(), "--nocapture".into()]
        ));
        assert!(has_run_command_capability(
            &caps,
            &["cargo".into(), "check".into()]
        ));
        assert!(!has_run_command_capability(
            &caps,
            &["cargo".into(), "fmt".into()]
        ));
    }

    #[test]
    fn no_run_command_capability() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });

        assert!(!has_run_command_capability(
            &caps,
            &["cargo".into(), "test".into()]
        ));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::audit::CepAuditContext;
    use crate::policy::default::PolicyConfig;
    use crate::policy::CepTurnContext;
    use crate::registry::{Cancellation, EventSink};
    use lattice_cep_protocol::envelope::{BaseExecutionEnvelope, ExecutionEnvelope, Intent, Scope};
    use lattice_cep_protocol::policy::PolicyBudgetSnapshot;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_handler_context(
        root: &str,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(DefaultPolicyHook::new(
            PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(root)),
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "policy test"),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
                active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lattice_cep_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    fn base_only_envelope() -> BaseExecutionEnvelope {
        BaseExecutionEnvelope::for_workspace_root(".", "test")
    }

    fn envelope_record(envelope_id: &str) -> EnvelopeRecord {
        EnvelopeRecord {
            envelope_id: envelope_id.into(),
            envelope_hash: "hash".into(),
            envelope: ExecutionEnvelope {
                intent: Intent {
                    description: "test".into(),
                },
                capabilities: BTreeSet::from([Capability::WritePath {
                    path_pattern: "src/**".into(),
                }]),
                scope: Scope::default(),
                impact: vec![],
                constraints: Default::default(),
                verify: None,
                on_failure: None,
                extensions: Default::default(),
            },
            declared_at_call_id: 0,
            supersedes: None,
            reason: None,
        }
    }

    #[test]
    fn snapshot_base_only() {
        let base = base_only_envelope();
        let snapshot = build_effective_envelope_snapshot(&base, None);
        assert_eq!(snapshot.base_envelope_id, "base");
        assert!(snapshot.declared_envelope_id.is_none());
        for sc in &snapshot.capabilities {
            assert_eq!(sc.source, CapabilitySource::Base);
        }
    }

    #[test]
    fn snapshot_base_plus_declared_no_overlap() {
        let base = base_only_envelope();
        let record = envelope_record("declared-1");
        let snapshot = build_effective_envelope_snapshot(&base, Some(&record));
        assert_eq!(snapshot.declared_envelope_id, Some("declared-1".into()));
        let has_declared = snapshot
            .capabilities
            .iter()
            .any(|sc| matches!(sc.source, CapabilitySource::Declared { .. }));
        assert!(has_declared, "should have declared-only capability");
    }

    #[test]
    fn snapshot_both_overlap() {
        let mut base = base_only_envelope();
        base.inner.capabilities.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        let record = envelope_record("declared-1");
        let snapshot = build_effective_envelope_snapshot(&base, Some(&record));
        let has_both = snapshot
            .capabilities
            .iter()
            .any(|sc| matches!(sc.source, CapabilitySource::Both { .. }));
        assert!(has_both, "overlapping capability should have Both source");
    }

    #[tokio::test]
    async fn build_policy_input_includes_session_id_and_budget() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let (ctx, _events) = test_handler_context(root);
        let input = build_policy_input(&ctx, "workspace.read_file", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(input.method, "workspace.read_file");
        assert_eq!(input.audit_context.turn_id, "test-turn");
        assert_eq!(input.audit_context.session_id, "test-session");
        assert!(
            input
                .audit_context
                .budget_snapshot
                .turn_observation_bytes_limit
                > 0
        );
    }

    #[test]
    fn evaluate_with_default_hook_returns_evidence() {
        let hook = DefaultPolicyHook::new(PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from("."),
        ));
        let input = PolicyInput {
            method: "workspace.read_file".into(),
            params: serde_json::json!({}),
            envelope_snapshot: EffectiveEnvelopeSnapshot {
                base_envelope_id: "base".into(),
                declared_envelope_id: None,
                capabilities: vec![],
            },
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: Some(1),
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 65536,
                    todo_ops_used: 0,
                    todo_ops_limit: 10,
                },
            },
        };
        let eval = evaluate_policy(&hook, Some(&hook), &input).unwrap();
        assert!(
            eval.evidence.is_some(),
            "default hook should return evidence"
        );
    }

    #[test]
    fn evaluate_with_external_hook_returns_no_evidence() {
        struct AlwaysAllowed;
        impl PolicyHook for AlwaysAllowed {
            fn decide(&self, _input: &PolicyInput) -> PolicyDecision {
                PolicyDecision::Allowed
            }
        }
        let hook = AlwaysAllowed;
        let input = PolicyInput {
            method: "test".into(),
            params: serde_json::json!({}),
            envelope_snapshot: EffectiveEnvelopeSnapshot {
                base_envelope_id: "base".into(),
                declared_envelope_id: None,
                capabilities: vec![],
            },
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: None,
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 0,
                    todo_ops_used: 0,
                    todo_ops_limit: 0,
                },
            },
        };
        let eval = evaluate_policy(&hook, None, &input).unwrap();
        assert!(
            eval.evidence.is_none(),
            "external hook should return no evidence"
        );
        assert_eq!(eval.decision, PolicyDecision::Allowed);
    }

    #[test]
    fn map_allowed_returns_ok() {
        assert!(map_decision_to_cep_error(&PolicyDecision::Allowed, "test").is_ok());
    }

    #[test]
    fn map_missing_capability_to_capability_not_granted() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            message: "no cap".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
        assert_eq!(err.retriability, Retriability::Permanent);
    }

    #[test]
    fn map_replan_required_to_replan_required() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::ReplanRequired,
            message: "replan".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_command_denied_to_guard_denied() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::CommandDenied,
            message: "denied".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert_eq!(err.retriability, Retriability::Permanent);
    }

    #[test]
    fn map_budget_exceeded_to_reserved_with_replan() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            message: "budget".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_non_determinable_to_guard_denied_unknown() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "unknown".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert_eq!(err.retriability, Retriability::Unknown);
    }

    #[test]
    fn map_human_review_to_guard_denied_unknown() {
        let decision = PolicyDecision::HumanReview {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "review".into(),
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert_eq!(err.retriability, Retriability::Unknown);
    }

    #[test]
    fn map_network_denied_to_guard_denied() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NetworkDenied,
            message: "blocked".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "web.fetch").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn map_path_outside_scope_to_replan_required() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PathOutsideScope,
            message: "outside".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_capability_source_not_allowed_to_capability_not_granted() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::CapabilitySourceNotAllowed,
            message: "source".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[test]
    fn map_policy_config_invalid_to_reserved() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PolicyConfigInvalid,
            message: "bad config".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
        assert_eq!(err.retriability, Retriability::Permanent);
    }

    #[test]
    fn map_envelope_required_to_capability_not_granted() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::EnvelopeRequired,
            message: "need envelope".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[test]
    fn verdict_allowed() {
        assert_eq!(
            decision_to_guard_verdict(&PolicyDecision::Allowed),
            GuardVerdict::Allowed
        );
    }

    #[test]
    fn verdict_denied() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            message: "no cap".into(),
            action_required: None,
        };
        assert_eq!(
            decision_to_guard_verdict(&decision),
            GuardVerdict::Denied {
                reason: "no cap".into()
            }
        );
    }

    #[test]
    fn verdict_human_review() {
        let decision = PolicyDecision::HumanReview {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "review needed".into(),
        };
        assert_eq!(
            decision_to_guard_verdict(&decision),
            GuardVerdict::HumanReview {
                reason: "review needed".into()
            }
        );
    }
}
