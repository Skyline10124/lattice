// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Verify flow coordination for workspace mutations.
//!
//! After every successful mutation call, the active envelope's `verify` command
//! is executed if configured. Verify failure returns `VerifyFailed`, retains the
//! diff, and leaves the envelope valid for same-turn repair.

use crate::execution_backend::{
    DirectVerifyConfig, ExecRequest, ExecutionBackendError, ExecutionRouting, PathFacts,
    PathIntent, PathKind, RoutingInvocationState,
};
use crate::executor::shell::{ExecuteConfig, execute_argv};
use crate::executor::supervisor::overlay::{
    CandidateArtifactView, VerifyDiscardPlan, VerifyDiscardReport, VerifyLayerStatus,
    cleanup_verify_discard_layer, mount_overlay,
};
use crate::executor::supervisor::syscall::{LiveSupervisorSyscalls, SupervisorSyscalls};
use crate::server::registry::Cancellation;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::{DirectPathFacts, DirectPathKind, validate_direct_path_facts};
use lancet_authority::policy::command::{
    CommandPolicy, EffectiveImpact, build_allowed_params_env, classify_command,
};
use lancet_protocol::envelope::ExecutionEnvelope;
use lancet_protocol::error::{
    CepErrorData, ExecutionErrorData, GuardErrorData, ValidationErrorData,
};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Parsed verify command specification extracted from an envelope.
#[derive(Clone, Debug)]
pub struct VerifyCommandSpec {
    /// Command argv tokens.
    pub argv: Vec<String>,
    /// Working directory (defaults to project root).
    pub cwd: Option<String>,
    /// Timeout in milliseconds (defaults to min(default_timeout, 60000)).
    pub timeout_ms: Option<u64>,
    /// Expected exit code for success determination.
    pub expected_exit_code: Option<i32>,
}

/// Outcome of a verify execution.
#[derive(Clone, Debug)]
pub struct VerifyOutcome {
    /// Whether the verify command succeeded.
    pub success: bool,
    /// Exit code of the verify command.
    pub exit_code: i32,
    /// Summary of stdout (may be truncated).
    pub stdout_summary: Option<String>,
    /// Summary of stderr (may be truncated).
    pub stderr_summary: Option<String>,
}

/// Artifact verify result with proof that verify writes were discarded.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub(crate) struct ArtifactVerifyOutcome {
    pub(crate) verify: VerifyOutcome,
    pub(crate) discard: VerifyDiscardReport,
}

/// Build the typed CEP error returned when verify executes but fails.
pub fn verify_failed_error(spec: &VerifyCommandSpec, outcome: &VerifyOutcome) -> CepError {
    CepError::new(
        CepErrorCode::VerifyFailed,
        format!(
            "verify command failed with exit code {}; diff retained",
            outcome.exit_code
        ),
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: spec.argv.join(" "),
            exit_code: Some(outcome.exit_code),
            stderr: outcome.stderr_summary.clone(),
        }),
        // Not RequiresReplan: the active envelope remains valid for same-scope repair.
        Retriability::Unknown,
    )
}

/// Extract verify command from envelope, if configured.
pub fn extract_verify_command(envelope: &ExecutionEnvelope) -> Option<VerifyCommandSpec> {
    envelope.verify.as_ref().map(|vc| VerifyCommandSpec {
        argv: vc.command.split_whitespace().map(String::from).collect(),
        cwd: None,        // defaults to project root
        timeout_ms: None, // defaults to min(default, 60000)
        expected_exit_code: vc.expected_exit_code,
    })
}

/// Validate a verify command at declare-time.
///
/// Verify commands must be diagnostic/verification-class only. WorkspaceMutation
/// and RiskyOrUnknown commands are rejected. Also checks cwd, timeout, and env keys.
pub fn validate_verify_command(
    spec: &VerifyCommandSpec,
    policy: &CommandPolicy,
    project_root: &str,
) -> Result<(), CepError> {
    // Validate argv is non-empty
    if spec.argv.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "verify command must have a non-empty argv",
            CepErrorData::Validation(ValidationErrorData {
                field: "verify.command".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Classify the verify command — must be Diagnostic
    let impact = classify_command(&spec.argv, None, policy)?;
    match impact {
        EffectiveImpact::Diagnostic => {} // OK
        EffectiveImpact::WorkspaceMutation => {
            return Err(CepError::new(
                CepErrorCode::GuardDenied,
                "verify command must be diagnostic/verification-class, not WorkspaceMutation",
                CepErrorData::Guard(GuardErrorData {
                    method: "session.envelope".into(),
                    reason: "verify command classified as WorkspaceMutation".into(),
                }),
                Retriability::Permanent,
            ));
        }
        EffectiveImpact::HardDenied => {
            return Err(CepError::new(
                CepErrorCode::GuardDenied,
                "verify command is hard-denied by command policy",
                CepErrorData::Guard(GuardErrorData {
                    method: "session.envelope".into(),
                    reason: "verify command classified as HardDenied".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    // Validate cwd is inside project root
    if let Some(ref cwd) = spec.cwd {
        let cwd_path = PathBuf::from(cwd);
        if cwd_path.is_absolute() {
            let canonical = std::fs::canonicalize(&cwd_path).map_err(|e| {
                CepError::new(
                    CepErrorCode::PathContainment,
                    format!("verify cwd '{}' cannot be canonicalized: {e}", cwd),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "verify.cwd".into(),
                        reason: e.to_string(),
                    }),
                    Retriability::Permanent,
                )
            })?;
            let root_canonical = std::fs::canonicalize(project_root).map_err(|e| {
                CepError::new(
                    CepErrorCode::ProjectRootViolation,
                    format!("cannot canonicalize project root: {e}"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "project_root".into(),
                        reason: e.to_string(),
                    }),
                    Retriability::Permanent,
                )
            })?;
            if canonical != root_canonical && !canonical.starts_with(&root_canonical) {
                return Err(CepError::new(
                    CepErrorCode::ProjectRootViolation,
                    format!("verify cwd '{}' is outside project root", cwd),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "verify.cwd".into(),
                        reason: "outside project root".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }

    // Validate timeout
    let effective_timeout = spec
        .timeout_ms
        .unwrap_or(std::cmp::min(policy.default_timeout_ms, 60_000));
    if effective_timeout > policy.max_timeout_ms {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            format!(
                "verify timeout {}ms exceeds maximum {}ms",
                effective_timeout, policy.max_timeout_ms
            ),
            CepErrorData::Validation(ValidationErrorData {
                field: "verify.timeout_ms".into(),
                reason: format!(
                    "requested {}ms > max {}ms",
                    effective_timeout, policy.max_timeout_ms
                ),
            }),
            Retriability::Permanent,
        ));
    }

    Ok(())
}

/// Run a verify command after a successful mutation.
///
/// Uses command-runner safety machinery (timeout, kill/reap, output cap).
/// Does NOT require a second active envelope.
/// Does NOT route as normal LLM-issued exec.command_run.
/// Does NOT recursively trigger verify.
pub async fn run_verify(
    spec: &VerifyCommandSpec,
    project_root: &str,
    policy: &CommandPolicy,
    cancellation: &mut Cancellation,
) -> Result<VerifyOutcome, CepError> {
    run_verify_at_project_root(spec, project_root, policy, cancellation).await
}

/// Run artifact verify against an accepted candidate artifact view.
///
/// This is native-private and intentionally bypasses public `exec.command_run`
/// routing to avoid recursive verify. The verify lower is the accepted
/// candidate tree; any verify writes land in a separate layer and are discarded
/// before callers can proceed to final lower-baseline validation or commit.
#[allow(dead_code)]
pub(crate) async fn run_verify_on_candidate_view(
    spec: &VerifyCommandSpec,
    candidate: &CandidateArtifactView,
    policy: &CommandPolicy,
    cancellation: &mut Cancellation,
    verify_session_id: &str,
) -> Result<ArtifactVerifyOutcome, CepError> {
    let mut syscalls = LiveSupervisorSyscalls::default();
    run_verify_on_candidate_view_with_syscalls(
        spec,
        candidate,
        policy,
        cancellation,
        verify_session_id,
        &mut syscalls,
    )
    .await
}

#[allow(dead_code)]
async fn run_verify_on_candidate_view_with_syscalls<S: SupervisorSyscalls>(
    spec: &VerifyCommandSpec,
    candidate: &CandidateArtifactView,
    policy: &CommandPolicy,
    cancellation: &mut Cancellation,
    verify_session_id: &str,
    syscalls: &mut S,
) -> Result<ArtifactVerifyOutcome, CepError> {
    let candidate_root = candidate.root.to_str().ok_or_else(|| {
        CepError::internal_error(format!(
            "candidate artifact view path '{}' is not valid UTF-8",
            candidate.root.display()
        ))
    })?;
    validate_verify_command(spec, policy, candidate_root)?;

    let verify_plan =
        VerifyDiscardPlan::deterministic(candidate, verify_session_id).map_err(|err| {
            CepError::internal_error(format!(
                "failed to plan candidate verify discard layer for {}: {}",
                err.field, err.reason
            ))
        })?;

    if let Err(err) = mount_overlay(&verify_plan.overlay, syscalls) {
        let discard = cleanup_verify_discard_layer(&verify_plan, true, syscalls);
        return Err(verify_layer_setup_failed_error(err.to_string(), &discard));
    }

    let verify_root = verify_plan.merged_path().to_str().ok_or_else(|| {
        CepError::internal_error(format!(
            "verify candidate merged path '{}' is not valid UTF-8",
            verify_plan.merged_path().display()
        ))
    })?;
    let verify_result = run_verify_at_project_root(spec, verify_root, policy, cancellation).await;
    let discard = cleanup_verify_discard_layer(&verify_plan, true, syscalls);
    if discard.verify_layer == VerifyLayerStatus::CleanupFailed {
        return Err(verify_layer_cleanup_failed_error(&discard));
    }

    let verify = verify_result?;
    if !verify.success {
        return Err(verify_failed_error(spec, &verify));
    }

    Ok(ArtifactVerifyOutcome { verify, discard })
}

/// Normalize a Direct verify profile config into one Upper [`ExecRequest`].
pub fn normalize_direct_verify_request(
    config: &DirectVerifyConfig,
    policy: &CommandPolicy,
) -> Result<ExecRequest, CepError> {
    if config.argv.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "direct verify argv must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "verify.argv".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    let impact = classify_command(&config.argv, None, policy)?;
    match impact {
        EffectiveImpact::Diagnostic => {}
        EffectiveImpact::WorkspaceMutation => {
            return Err(CepError::new(
                CepErrorCode::GuardDenied,
                "direct verify command must be diagnostic/verification-class, not WorkspaceMutation",
                CepErrorData::Guard(GuardErrorData {
                    method: "verify".into(),
                    reason: "verify command classified as WorkspaceMutation".into(),
                }),
                Retriability::Permanent,
            ));
        }
        EffectiveImpact::HardDenied => {
            return Err(CepError::new(
                CepErrorCode::GuardDenied,
                "direct verify command is hard-denied by command policy",
                CepErrorData::Guard(GuardErrorData {
                    method: "verify".into(),
                    reason: "verify command classified as HardDenied".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    let requested_timeout = config
        .timeout_ms
        .unwrap_or(std::cmp::min(policy.default_timeout_ms, 60_000));
    let effective_timeout = std::cmp::min(requested_timeout, policy.max_timeout_ms);
    let env = build_allowed_params_env(&BTreeMap::new(), policy)?;
    ExecRequest::new(
        config.argv.clone(),
        env,
        effective_timeout,
        config.cwd.clone(),
    )
    .map_err(|_| {
        CepError::new(
            CepErrorCode::SchemaMismatch,
            "direct verify argv must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "verify.argv".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        )
    })
}

/// Run Direct-mode post-mutation verification exclusively through the Upper backend.
///
/// Adapter failures return a typed execution error (`BackendExecution` stage).
/// A completed non-success exit code returns `Ok` with `success = false`
/// (`VerificationFailure` stage).
pub async fn run_verify_with_backend(
    config: &DirectVerifyConfig,
    execution: &ExecutionRouting,
    command_policy: &CommandPolicy,
    cancellation: &mut Cancellation,
    routing: &RoutingInvocationState,
    authorized_region: &AuthorizedEffectRegion,
) -> Result<VerifyOutcome, CepError> {
    if cancellation.is_cancelled() {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            "direct verify cancelled before adapter preflight",
            CepErrorData::Execution(ExecutionErrorData {
                command: config.argv.join(" "),
                exit_code: None,
                stderr: Some("cancelled".into()),
            }),
            Retriability::Permanent,
        ));
    }

    let request = normalize_direct_verify_request(config, command_policy)?;
    let cwd = request.cwd.clone();
    let cwd_str = cwd.as_str();

    routing.mark_adapter_preflight_invoked();
    let path_facts = execution
        .backend
        .resolve_path(&cwd, PathIntent::ExecCwd)
        .await
        .map_err(|err| verify_backend_error(&config.argv, err))?;

    validate_direct_verify_cwd(cwd_str, &path_facts, authorized_region)?;

    if cancellation.is_cancelled() {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            "direct verify cancelled before backend execution",
            CepErrorData::Execution(ExecutionErrorData {
                command: config.argv.join(" "),
                exit_code: None,
                stderr: Some("cancelled".into()),
            }),
            Retriability::Permanent,
        ));
    }

    routing.mark_backend_invoked();
    let output = execution
        .backend
        .exec(&request)
        .await
        .map_err(|err| verify_backend_error(&config.argv, err))?;

    let success = output.exit_code == config.expected_exit_code;
    Ok(VerifyOutcome {
        success,
        exit_code: output.exit_code,
        stdout_summary: bounded_summary(&output.stdout, command_policy.max_output_bytes),
        stderr_summary: bounded_summary(&output.stderr, command_policy.max_output_bytes),
    })
}

fn validate_direct_verify_cwd(
    path: &str,
    facts: &PathFacts,
    authorized_region: &AuthorizedEffectRegion,
) -> Result<(), CepError> {
    let direct_facts = path_facts_to_direct(facts);
    let empty_region = AuthorizedEffectRegion::empty();
    let region = if path == "." {
        &empty_region
    } else {
        authorized_region
    };
    validate_direct_path_facts(path, &direct_facts, region)
}

fn path_facts_to_direct(facts: &PathFacts) -> DirectPathFacts {
    DirectPathFacts {
        normalized_path: facts.normalized_path.clone(),
        exists: facts.exists,
        kind: facts.kind.map(path_kind_to_direct),
        parent_exists: facts.parent_exists,
        parent_kind: facts.parent_kind.map(path_kind_to_direct),
        canonical_under_workspace: facts.canonical_under_workspace,
    }
}

fn path_kind_to_direct(kind: PathKind) -> DirectPathKind {
    match kind {
        PathKind::File => DirectPathKind::File,
        PathKind::Directory => DirectPathKind::Directory,
        PathKind::Symlink => DirectPathKind::Symlink,
    }
}

fn verify_backend_error(argv: &[String], err: ExecutionBackendError) -> CepError {
    let command = argv.join(" ");
    match err {
        ExecutionBackendError::Path(path_err) => CepError::new(
            CepErrorCode::PathContainment,
            format!("direct verify cwd rejected: {path_err}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "verify.cwd".into(),
                reason: path_err.to_string(),
            }),
            Retriability::Permanent,
        ),
        ExecutionBackendError::Unavailable { message } => CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("direct verify backend unavailable: {message}"),
            CepErrorData::Execution(ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some(message),
            }),
            Retriability::Transient,
        ),
        ExecutionBackendError::ExecutionFailed { message } => CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("direct verify backend execution failed: {message}"),
            CepErrorData::Execution(ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some(message),
            }),
            Retriability::Transient,
        ),
        ExecutionBackendError::ContentTooLarge => CepError::new(
            CepErrorCode::ExecutionFailed,
            "direct verify output exceeded configured limits",
            CepErrorData::Execution(ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some("content too large".into()),
            }),
            Retriability::Permanent,
        ),
        ExecutionBackendError::Timeout => CepError::new(
            CepErrorCode::ExecutionFailed,
            "direct verify timed out",
            CepErrorData::Execution(ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some("timeout".into()),
            }),
            Retriability::Transient,
        ),
    }
}

fn bounded_summary(output: &str, max_bytes: usize) -> Option<String> {
    if output.is_empty() {
        return None;
    }
    if output.len() <= max_bytes {
        return Some(output.to_string());
    }
    let truncated: String = output.chars().take(max_bytes).collect();
    Some(truncated)
}

async fn run_verify_at_project_root(
    spec: &VerifyCommandSpec,
    project_root: &str,
    policy: &CommandPolicy,
    cancellation: &mut Cancellation,
) -> Result<VerifyOutcome, CepError> {
    let effective_timeout = spec
        .timeout_ms
        .unwrap_or(std::cmp::min(policy.default_timeout_ms, 60_000));

    // Build effective env: minimal, just PATH from inherited
    let mut env = BTreeMap::new();
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".to_string(), path);
    }

    let (output, _stdout_meta, _stderr_meta) = execute_argv(ExecuteConfig {
        project_root,
        argv: &spec.argv,
        cwd: spec.cwd.as_deref(),
        env: &env,
        timeout_ms: effective_timeout,
        cancellation,
        max_output_bytes: policy.max_output_bytes,
    })
    .await?;

    let exit_code = output.status.code().unwrap_or(-1);
    let expected = spec.expected_exit_code.unwrap_or(0);
    let success = exit_code == expected;

    Ok(VerifyOutcome {
        success,
        exit_code,
        stdout_summary: Some(String::from_utf8_lossy(&output.stdout).to_string()),
        stderr_summary: Some(String::from_utf8_lossy(&output.stderr).to_string()),
    })
}

#[allow(dead_code)]
fn verify_layer_setup_failed_error(reason: String, discard: &VerifyDiscardReport) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!(
            "candidate verify discard layer setup failed: {reason}; verify_layer={}",
            discard.verify_layer.label()
        ),
        CepErrorData::Execution(ExecutionErrorData {
            command: "artifact.verify".into(),
            exit_code: None,
            stderr: Some(reason),
        }),
        Retriability::Transient,
    )
}

#[allow(dead_code)]
fn verify_layer_cleanup_failed_error(discard: &VerifyDiscardReport) -> CepError {
    let reason = discard
        .reason
        .clone()
        .unwrap_or_else(|| "candidate verify discard cleanup failed".to_string());
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!(
            "candidate verify discard cleanup failed; verify_layer={}",
            discard.verify_layer.label()
        ),
        CepErrorData::Execution(ExecutionErrorData {
            command: "artifact.verify".into(),
            exit_code: None,
            stderr: Some(reason),
        }),
        Retriability::Transient,
    )
}

#[cfg(test)]
mod verify_tests {
    use super::*;
    use crate::executor::supervisor::artifact::{
        ArtifactEntry, ArtifactManifest, ArtifactOperation, RegularArtifactEntry,
    };
    use crate::executor::supervisor::landlock::{LandlockAbiProbe, LandlockApplicationReport};
    use crate::executor::supervisor::overlay::{
        OverlayMode, OverlayPlan, prepare_candidate_artifact_view,
    };
    use crate::executor::supervisor::plan::ReadOnlyBindScope;
    use crate::executor::supervisor::syscall::SyscallError;
    use lancet_authority::policy::CommandPolicy;
    use std::collections::BTreeSet;
    use std::path::Path;
    use tempfile::TempDir;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn default_policy() -> CommandPolicy {
        CommandPolicy::defaults()
    }

    fn diagnostic_envelope() -> ExecutionEnvelope {
        use lancet_protocol::envelope::*;
        use std::collections::BTreeSet;
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: "test".into(),
            },
            capabilities: caps,
            scope: Scope::default(),
            impact: vec![],
            constraints: Constraints::default(),
            verify: Some(VerifyCommand {
                command: "echo ok".into(),
                expected_exit_code: Some(0),
            }),
            on_failure: Some(OnFailure::RetainDiff),
            extensions: serde_json::Map::new(),
        }
    }

    fn accepted_manifest(path: &str) -> ArtifactManifest {
        ArtifactManifest {
            entries: vec![ArtifactEntry::Regular(RegularArtifactEntry {
                path: path.to_string(),
                operation: ArtifactOperation::Create,
                content_hash: "blake3:accepted".to_string(),
                size_bytes: 8,
                mode: 0o644,
                lower_hash: None,
            })],
        }
    }

    fn candidate_fixture(path: &str, content: &str) -> (TempDir, OverlayPlan, ArtifactManifest) {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let artifact_plan = OverlayPlan::deterministic(&project, OverlayMode::Artifact, "artifact")
            .expect("artifact plan");
        std::fs::create_dir_all(&artifact_plan.paths.upper).unwrap();
        std::fs::create_dir_all(&artifact_plan.paths.work).unwrap();
        std::fs::create_dir_all(&artifact_plan.paths.merged).unwrap();
        let candidate_file = artifact_plan.paths.merged.join(path);
        if let Some(parent) = candidate_file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(candidate_file, content).unwrap();
        (temp, artifact_plan, accepted_manifest(path))
    }

    #[cfg(unix)]
    fn executable_script(temp: &TempDir, name: &str, body: &str) -> String {
        let path = temp.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path.to_string_lossy().to_string()
    }

    fn policy_allowing(script: &str) -> CommandPolicy {
        let mut policy = default_policy();
        policy
            .allowed_diagnostic_commands
            .push(vec![script.to_string()]);
        policy
    }

    #[derive(Default)]
    struct FakeVerifySyscalls {
        calls: Vec<String>,
        fail: BTreeSet<&'static str>,
    }

    impl FakeVerifySyscalls {
        fn fail(mut self, operation: &'static str) -> Self {
            self.fail.insert(operation);
            self
        }

        fn maybe_fail(&self, operation: &'static str) -> Result<(), SyscallError> {
            if self.fail.contains(operation) {
                Err(SyscallError::new(operation, "injected failure"))
            } else {
                Ok(())
            }
        }
    }

    impl SupervisorSyscalls for FakeVerifySyscalls {
        fn has_cap_sys_admin(&mut self) -> Result<bool, SyscallError> {
            Ok(true)
        }
        fn check_unprivileged_user_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_user_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_setgroups_deny(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_uid_map_current(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn write_gid_map_current(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn enter_pid_namespace_child(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_network_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn assert_no_veth_or_default_route(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn unshare_mount_namespace(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn make_mounts_private(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_tmpfs_root(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn bind_mount(&mut self, _source: &Path, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn remount_read_only(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn record_read_only_bind_scope(
            &mut self,
            _scope: ReadOnlyBindScope,
        ) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_scratch_tmpfs(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_procfs(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_minimal_device(
            &mut self,
            _source: &Path,
            _target: &Path,
        ) -> Result<(), SyscallError> {
            Ok(())
        }
        fn detach_old_root(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn assert_old_root_unreachable(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn chdir(&mut self, _target: &Path) -> Result<(), SyscallError> {
            Ok(())
        }
        fn cleanup_env(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn cleanup_fds(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn probe_landlock_abi(&mut self) -> Result<LandlockAbiProbe, SyscallError> {
            Ok(LandlockAbiProbe::unsupported("not tested"))
        }
        fn apply_landlock(
            &mut self,
            _plan: &crate::executor::supervisor::landlock::LandlockRefinementPlan,
        ) -> Result<LandlockApplicationReport, SyscallError> {
            Err(SyscallError::new("landlock", "not tested"))
        }
        fn setup_report_pipe_ready(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn exec_handoff_ready(&mut self) -> Result<(), SyscallError> {
            Ok(())
        }
        fn mount_overlay(
            &mut self,
            lower: &Path,
            _upper: &Path,
            _work: &Path,
            merged: &Path,
        ) -> Result<(), SyscallError> {
            self.calls.push(format!(
                "mount_overlay:{}:{}",
                lower.display(),
                merged.display()
            ));
            self.maybe_fail("mount_overlay")?;
            copy_tree(lower, merged).map_err(|err| SyscallError::new("copy_tree", err))?;
            Ok(())
        }
        fn umount_overlay(&mut self, merged: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("umount_overlay:{}", merged.display()));
            self.maybe_fail("umount_overlay")
        }
        fn remove_overlay_tree(&mut self, path: &Path) -> Result<(), SyscallError> {
            self.calls
                .push(format!("remove_overlay_tree:{}", path.display()));
            if path.ends_with("upper") {
                self.maybe_fail("remove_upper")?;
            } else if path.ends_with("work") {
                self.maybe_fail("remove_work")?;
            } else if path.ends_with("merged") {
                self.maybe_fail("remove_merged")?;
            } else {
                self.maybe_fail("remove_session_root")?;
            }
            std::fs::remove_dir_all(path)
                .map_err(|err| SyscallError::new("remove_overlay_tree", err.to_string()))
        }
    }

    fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
        std::fs::create_dir_all(to).map_err(|err| err.to_string())?;
        for entry in std::fs::read_dir(from).map_err(|err| err.to_string())? {
            let entry = entry.map_err(|err| err.to_string())?;
            let source = entry.path();
            let target = to.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(&source).map_err(|err| err.to_string())?;
            if metadata.is_dir() {
                copy_tree(&source, &target)?;
            } else if metadata.is_file() {
                std::fs::copy(&source, &target).map_err(|err| err.to_string())?;
            }
        }
        Ok(())
    }

    #[test]
    fn extract_verify_command_from_envelope() {
        let env = diagnostic_envelope();
        let spec = extract_verify_command(&env).unwrap();
        assert_eq!(spec.argv, vec!["echo", "ok"]);
        assert_eq!(spec.expected_exit_code, Some(0));
    }

    #[test]
    fn extract_verify_command_returns_none_when_not_configured() {
        use lancet_protocol::envelope::*;
        use std::collections::BTreeSet;
        let env = ExecutionEnvelope {
            intent: Intent {
                description: "no verify".into(),
            },
            capabilities: BTreeSet::new(),
            scope: Scope::default(),
            impact: vec![],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        };
        assert!(extract_verify_command(&env).is_none());
    }

    #[test]
    fn validate_diagnostic_verify_command_succeeds() {
        let policy = default_policy();
        let spec = VerifyCommandSpec {
            argv: vec!["echo".into(), "ok".into()],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: Some(0),
        };
        let result = validate_verify_command(&spec, &policy, ".");
        assert!(result.is_ok());
    }

    #[test]
    fn validate_workspace_mutation_verify_command_rejected() {
        let policy = default_policy();
        let spec = VerifyCommandSpec {
            argv: vec!["cargo".into(), "fmt".into()],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: Some(0),
        };
        let err = validate_verify_command(&spec, &policy, ".").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn validate_hard_denied_verify_command_rejected() {
        let policy = default_policy();
        let spec = VerifyCommandSpec {
            argv: vec!["sh".into(), "-c".into(), "echo bad".into()],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: Some(0),
        };
        let err = validate_verify_command(&spec, &policy, ".").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn validate_empty_argv_rejected() {
        let policy = default_policy();
        let spec = VerifyCommandSpec {
            argv: vec![],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: None,
        };
        let err = validate_verify_command(&spec, &policy, ".").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn run_verify_echo_succeeds() {
        let policy = default_policy();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let spec = VerifyCommandSpec {
            argv: vec!["echo".into(), "ok".into()],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: Some(0),
        };
        let outcome = run_verify(&spec, ".", &policy, &mut cancel).await.unwrap();
        assert!(outcome.success);
        assert_eq!(outcome.exit_code, 0);
    }

    #[tokio::test]
    async fn run_verify_wrong_exit_code_is_failure() {
        let policy = default_policy();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let spec = VerifyCommandSpec {
            argv: vec!["false".into()],
            cwd: None,
            timeout_ms: None,
            expected_exit_code: Some(0),
        };
        let outcome = run_verify(&spec, ".", &policy, &mut cancel).await.unwrap();
        assert!(!outcome.success);
        assert_eq!(outcome.exit_code, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_verify_candidate_reads_file_only_in_candidate_view() {
        let (temp, artifact_plan, manifest) = candidate_fixture("candidate-only.txt", "candidate");
        let script = executable_script(&temp, "read-candidate", "cat candidate-only.txt");
        let policy = policy_allowing(&script);
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancellation = Cancellation::new(cancel_rx);
        let mut syscalls = FakeVerifySyscalls::default();

        let outcome = run_verify_on_candidate_view_with_syscalls(
            &VerifyCommandSpec {
                argv: vec![script],
                cwd: None,
                timeout_ms: None,
                expected_exit_code: Some(0),
            },
            &candidate,
            &policy,
            &mut cancellation,
            "verify-read",
            &mut syscalls,
        )
        .await
        .unwrap();

        assert!(outcome.verify.success);
        assert_eq!(outcome.discard.verify_layer, VerifyLayerStatus::Discarded);
        assert_eq!(outcome.verify.stdout_summary.as_deref(), Some("candidate"));
        assert!(
            !artifact_plan
                .paths
                .lower
                .join("candidate-only.txt")
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_verify_discard_excludes_verify_writes_from_manifest_and_workspace() {
        let (temp, artifact_plan, manifest) = candidate_fixture("accepted.txt", "accepted");
        let script = executable_script(&temp, "write-verify", "printf verify > verify-only.txt");
        let policy = policy_allowing(&script);
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);
        let manifest_before = candidate.manifest.clone();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancellation = Cancellation::new(cancel_rx);
        let mut syscalls = FakeVerifySyscalls::default();

        let outcome = run_verify_on_candidate_view_with_syscalls(
            &VerifyCommandSpec {
                argv: vec![script],
                cwd: None,
                timeout_ms: None,
                expected_exit_code: Some(0),
            },
            &candidate,
            &policy,
            &mut cancellation,
            "verify-write",
            &mut syscalls,
        )
        .await
        .unwrap();

        assert!(outcome.verify.success);
        assert_eq!(outcome.discard.verify_layer, VerifyLayerStatus::Discarded);
        assert_eq!(candidate.manifest, manifest_before);
        assert_eq!(candidate.manifest_paths(), vec!["accepted.txt"]);
        assert!(
            !candidate
                .manifest_paths()
                .contains(&"verify-only.txt".to_string())
        );
        assert!(!artifact_plan.paths.lower.join("verify-only.txt").exists());
        assert!(!artifact_plan.paths.upper.join("verify-only.txt").exists());
        assert!(!candidate.root.join("verify-only.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_verify_discard_failure_returns_verify_failed_after_discard() {
        let (temp, artifact_plan, manifest) = candidate_fixture("accepted.txt", "accepted");
        let script = executable_script(
            &temp,
            "fail-verify",
            "printf verify > verify-only.txt\nexit 7",
        );
        let policy = policy_allowing(&script);
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancellation = Cancellation::new(cancel_rx);
        let mut syscalls = FakeVerifySyscalls::default();

        let err = run_verify_on_candidate_view_with_syscalls(
            &VerifyCommandSpec {
                argv: vec![script],
                cwd: None,
                timeout_ms: None,
                expected_exit_code: Some(0),
            },
            &candidate,
            &policy,
            &mut cancellation,
            "verify-fail",
            &mut syscalls,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, CepErrorCode::VerifyFailed);
        assert!(!artifact_plan.paths.lower.join("verify-only.txt").exists());
        assert!(!artifact_plan.paths.upper.join("verify-only.txt").exists());
        assert!(!candidate.root.join("verify-only.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_verify_discard_cleanup_failure_blocks_candidate_success() {
        let (temp, artifact_plan, manifest) = candidate_fixture("accepted.txt", "accepted");
        let script = executable_script(&temp, "pass-verify", "echo ok");
        let policy = policy_allowing(&script);
        let candidate = prepare_candidate_artifact_view(&artifact_plan, &manifest);
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancellation = Cancellation::new(cancel_rx);
        let mut syscalls = FakeVerifySyscalls::default().fail("remove_upper");

        let err = run_verify_on_candidate_view_with_syscalls(
            &VerifyCommandSpec {
                argv: vec![script],
                cwd: None,
                timeout_ms: None,
                expected_exit_code: Some(0),
            },
            &candidate,
            &policy,
            &mut cancellation,
            "verify-cleanup-fail",
            &mut syscalls,
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    }

    mod phase34_direct_verify {
        use super::*;
        use crate::execution_backend::{
            DirectoryEntry, ExecOutput, ExecRequest, ExecutionBackend, ExecutionBackendError,
            ExecutionRouting, FindRequest, GrepMatch, GrepRequest, PathFacts, PathIntent,
            RoutingInvocationState, UpperContext, WorkspacePath,
        };
        use lancet_authority::effect::region::AuthorizedEffectRegion;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct VerifySpyBackend {
            resolve_calls: Mutex<Vec<(String, PathIntent)>>,
            exec_calls: Mutex<Vec<ExecRequest>>,
            resolve_result: Mutex<Option<Result<PathFacts, ExecutionBackendError>>>,
            exec_result: Mutex<Option<Result<ExecOutput, ExecutionBackendError>>>,
        }

        impl VerifySpyBackend {
            fn cwd_facts(path: &str) -> PathFacts {
                PathFacts {
                    normalized_path: path.to_string(),
                    exists: true,
                    kind: Some(crate::execution_backend::PathKind::Directory),
                    parent_exists: true,
                    parent_kind: Some(crate::execution_backend::PathKind::Directory),
                    byte_size: None,
                    canonical_under_workspace: true,
                }
            }
        }

        impl ExecutionBackend for VerifySpyBackend {
            fn backend_name(&self) -> &str {
                "verify-spy"
            }

            fn resolve_path(
                &self,
                path: &WorkspacePath,
                intent: PathIntent,
            ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
            {
                self.resolve_calls
                    .lock()
                    .unwrap()
                    .push((path.as_str().to_string(), intent));
                let result = self
                    .resolve_result
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| Ok(Self::cwd_facts(path.as_str())));
                Box::pin(async move { result })
            }

            fn read_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn write_file(
                &self,
                _path: &WorkspacePath,
                _content: &[u8],
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn remove_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn list_dir(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn grep(
                &self,
                _request: &GrepRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn find_files(
                &self,
                _request: &FindRequest,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn exec(
                &self,
                request: &ExecRequest,
            ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, ExecutionBackendError>> + Send + '_>>
            {
                self.exec_calls.lock().unwrap().push(request.clone());
                let result = self
                    .exec_result
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(Ok(ExecOutput {
                        exit_code: 0,
                        stdout: "ok".into(),
                        stderr: String::new(),
                    }));
                Box::pin(async move { result })
            }

            fn changed_paths(
                &self,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }
        }

        fn routing_with_spy() -> (ExecutionRouting, Arc<VerifySpyBackend>) {
            let spy = Arc::new(VerifySpyBackend::default());
            let execution = ExecutionRouting {
                backend: spy.clone(),
                upper: UpperContext {
                    execution_backend: "verify-spy".into(),
                    upper_workspace_id: "upper-verify".into(),
                },
            };
            (execution, spy)
        }

        fn direct_config() -> DirectVerifyConfig {
            DirectVerifyConfig {
                argv: vec!["echo".into(), "ok".into()],
                cwd: WorkspacePath::project_root(),
                timeout_ms: Some(5_000),
                expected_exit_code: 0,
            }
        }

        #[tokio::test]
        async fn phase34_direct_verify_uses_backend_argv_env_timeout_cwd() {
            let policy = default_policy();
            let (execution, spy) = routing_with_spy();
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();
            let region = AuthorizedEffectRegion::empty();

            let outcome = run_verify_with_backend(
                &direct_config(),
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &region,
            )
            .await
            .unwrap();

            assert!(outcome.success);
            let exec_calls = spy.exec_calls.lock().unwrap();
            assert_eq!(exec_calls.len(), 1);
            assert_eq!(exec_calls[0].argv, vec!["echo", "ok"]);
            assert_eq!(exec_calls[0].cwd.as_str(), ".");
            assert_eq!(exec_calls[0].timeout_ms, 5_000);
            let resolve_calls = spy.resolve_calls.lock().unwrap();
            assert_eq!(resolve_calls.len(), 1);
            assert_eq!(resolve_calls[0].0, ".");
            assert_eq!(resolve_calls[0].1, PathIntent::ExecCwd);
            assert!(routing_state.adapter_preflight_invoked());
            assert!(routing_state.backend_invoked());
        }

        #[tokio::test]
        async fn phase34_direct_verify_omitted_cwd_uses_project_root_sentinel() {
            let policy = default_policy();
            let config = DirectVerifyConfig {
                argv: vec!["echo".into(), "root".into()],
                cwd: WorkspacePath::project_root(),
                timeout_ms: None,
                expected_exit_code: 0,
            };
            let (execution, spy) = routing_with_spy();
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();

            run_verify_with_backend(
                &config,
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap();

            let exec_calls = spy.exec_calls.lock().unwrap();
            assert_eq!(exec_calls[0].cwd.as_str(), ".");
            assert!(!exec_calls[0].cwd.as_str().starts_with('/'));
        }

        #[tokio::test]
        async fn phase34_direct_verify_cwd_preflight_denial_before_exec() {
            let policy = default_policy();
            let (execution, spy) = routing_with_spy();
            *spy.resolve_result.lock().unwrap() = Some(Ok(PathFacts {
                normalized_path: ".git".into(),
                exists: true,
                kind: Some(crate::execution_backend::PathKind::Directory),
                parent_exists: true,
                parent_kind: Some(crate::execution_backend::PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            }));
            let config = DirectVerifyConfig {
                argv: vec!["echo".into(), "bad".into()],
                cwd: WorkspacePath::parse(".git").unwrap(),
                timeout_ms: None,
                expected_exit_code: 0,
            };
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();

            let err = run_verify_with_backend(
                &config,
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap_err();

            assert_eq!(err.code, CepErrorCode::GuardDenied);
            assert!(routing_state.adapter_preflight_invoked());
            assert!(!routing_state.backend_invoked());
            assert!(spy.exec_calls.lock().unwrap().is_empty());
        }

        #[tokio::test]
        async fn phase34_direct_verify_cancellation_before_exec() {
            let policy = default_policy();
            let (execution, _spy) = routing_with_spy();
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            cancel_tx.send(true).unwrap();
            let routing_state = RoutingInvocationState::default();

            let err = run_verify_with_backend(
                &direct_config(),
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap_err();

            assert_eq!(err.code, CepErrorCode::ExecutionFailed);
            assert!(!routing_state.backend_invoked());
        }

        #[tokio::test]
        async fn phase34_direct_verify_nonzero_expected_exit_is_failure_outcome() {
            let policy = default_policy();
            let (execution, spy) = routing_with_spy();
            *spy.exec_result.lock().unwrap() = Some(Ok(ExecOutput {
                exit_code: 7,
                stdout: String::new(),
                stderr: "fail".into(),
            }));
            let config = DirectVerifyConfig {
                argv: vec!["echo".into(), "fail".into()],
                cwd: WorkspacePath::project_root(),
                timeout_ms: None,
                expected_exit_code: 0,
            };
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();

            let outcome = run_verify_with_backend(
                &config,
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap();

            assert!(!outcome.success);
            assert_eq!(outcome.exit_code, 7);
            assert!(routing_state.backend_invoked());
        }

        #[tokio::test]
        async fn phase34_direct_verify_adapter_error_is_distinct_from_verify_failure() {
            let policy = default_policy();
            let (execution, spy) = routing_with_spy();
            *spy.exec_result.lock().unwrap() = Some(Err(ExecutionBackendError::ExecutionFailed {
                message: "adapter blew up".into(),
            }));
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();

            let err = run_verify_with_backend(
                &direct_config(),
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap_err();

            assert_eq!(err.code, CepErrorCode::ExecutionFailed);
            assert_ne!(err.code, CepErrorCode::VerifyFailed);
            assert!(routing_state.backend_invoked());
            assert_eq!(spy.exec_calls.lock().unwrap().len(), 1);
        }

        static HOST_VERIFY_SENTINEL: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);

        #[tokio::test]
        async fn phase34_direct_verify_never_invokes_host_run_verify() {
            HOST_VERIFY_SENTINEL.store(false, std::sync::atomic::Ordering::SeqCst);
            struct HostVerifyGuard;
            impl Drop for HostVerifyGuard {
                fn drop(&mut self) {
                    if HOST_VERIFY_SENTINEL.load(std::sync::atomic::Ordering::SeqCst) {
                        panic!("host run_verify path must not be used in Direct verify");
                    }
                }
            }
            let _guard = HostVerifyGuard;
            let policy = default_policy();
            let (execution, _spy) = routing_with_spy();
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let routing_state = RoutingInvocationState::default();

            let outcome = run_verify_with_backend(
                &direct_config(),
                &execution,
                &policy,
                &mut cancel,
                &routing_state,
                &AuthorizedEffectRegion::empty(),
            )
            .await
            .unwrap();

            assert!(outcome.success);
        }
    }
}
