//! Verify flow coordination for workspace mutations.
//!
//! Implements D-43, D-44, D-45, D-46, D-47, D-48, D-49, D-50, D-51, D-52.
//!
//! After every successful mutation call, the active envelope's `verify` command
//! is executed if configured. Verify failure returns `VerifyFailed`, retains the
//! diff, and leaves the envelope valid for same-turn repair.

use crate::executor::shell::{execute_argv, ExecuteConfig};
use crate::policy::command::{classify_command, CommandPolicy, EffectiveImpact};
use crate::registry::Cancellation;
use lancet_protocol::envelope::ExecutionEnvelope;
use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
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
/// D-46: Must be diagnostic/verification-class only. Rejects WorkspaceMutation
/// or RiskyOrUnknown verify commands. Checks cwd, timeout, env keys.
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
                    method: "envelope.declare".into(),
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
                    method: "envelope.declare".into(),
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
/// D-47: Uses command-runner safety machinery (timeout, kill/reap, output cap).
/// Does NOT require a second active envelope.
/// Does NOT route as normal LLM-issued command.run.
/// Does NOT recursively trigger verify.
pub async fn run_verify(
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
        limits: &lattice_tooling::ToolLimits::default(),
        argv: &spec.argv,
        cwd: spec.cwd.as_deref(),
        env: &env,
        timeout_ms: Some(effective_timeout),
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
    use crate::policy::CommandPolicy;

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
}
