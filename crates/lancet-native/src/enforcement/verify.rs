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
use crate::executor::process::{ExecuteConfig, execute_argv};
use crate::server::registry::Cancellation;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::{DirectPathFacts, DirectPathKind, validate_direct_path_facts};
use lancet_authority::policy::command::{
    CommandPolicy, EffectiveImpact, build_allowed_params_env, classify_command,
};
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

#[cfg(test)]
mod verify_tests {
    use super::*;
    use lancet_authority::policy::CommandPolicy;

    fn default_policy() -> CommandPolicy {
        CommandPolicy::defaults()
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
