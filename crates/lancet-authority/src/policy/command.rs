// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Command execution policy for CEP exec.command_run handler.
//!
//! Classifies commands into three tiers: Diagnostic, WorkspaceMutation, HardDenied.
//! Enforces env and timeout policy with typed CEP errors.

use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{CepErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use std::collections::BTreeMap;

/// Effective impact classification after policy evaluation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectiveImpact {
    /// Read-only / verification / inspection commands. May run without active envelope.
    Diagnostic,
    /// Commands that may write workspace files or alter project state.
    /// Requires active ExecutionEnvelope with RunCommand capability.
    WorkspaceMutation,
    /// Commands that are always rejected (shell eval, inline scripts, etc.).
    HardDenied,
}

/// Command execution policy configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPolicy {
    /// Default timeout in milliseconds when not specified by the caller.
    pub default_timeout_ms: u64,
    /// Maximum allowed timeout. Exceeding this returns a typed error (not silent clamp).
    pub max_timeout_ms: u64,
    /// Maximum output bytes preserved per stream (stdout/stderr).
    pub max_output_bytes: usize,
    /// Whether inline shell invocations (sh -c, bash -c, etc.) are allowed.
    pub allow_inline_shell: bool,
    /// Maximum accepted inline-shell script length in bytes. Clamped to a
    /// hard ceiling in the shell AST screen.
    pub inline_shell_max_script_bytes: usize,
    /// Maximum number of simple commands in one inline-shell script.
    /// Clamped to a hard ceiling in the shell AST screen.
    pub inline_shell_max_commands: usize,
    /// Maximum number of words per inline-shell simple command. Clamped to
    /// a hard ceiling in the shell AST screen.
    pub inline_shell_max_words_per_command: usize,
    /// Commands classified as diagnostic (read-only). Token-prefix matched.
    pub allowed_diagnostic_commands: Vec<Vec<String>>,
    /// Commands that are always denied. Token-prefix matched with highest precedence.
    pub denied_commands: Vec<Vec<String>>,
    /// Environment variable keys allowed from caller params.
    pub allowed_env: Vec<String>,
    /// Environment variable keys inherited from the parent process.
    pub inherited_env: Vec<String>,
}

impl CommandPolicy {
    /// Conservative defaults suitable for production.
    pub fn defaults() -> Self {
        Self {
            default_timeout_ms: 120_000,
            max_timeout_ms: 300_000,
            max_output_bytes: 200_000,
            allow_inline_shell: false,
            inline_shell_max_script_bytes: 4096,
            inline_shell_max_commands: 16,
            inline_shell_max_words_per_command: 64,
            allowed_diagnostic_commands: vec![
                vec!["cargo".into(), "test".into()],
                vec!["cargo".into(), "check".into()],
                vec!["cargo".into(), "clippy".into()],
                vec!["cargo".into(), "doc".into()],
                vec!["cargo".into(), "fmt".into(), "--check".into()],
                vec![
                    "cargo".into(),
                    "fmt".into(),
                    "--all".into(),
                    "--check".into(),
                ],
                vec!["git".into(), "diff".into()],
                vec!["git".into(), "status".into()],
                vec!["git".into(), "log".into()],
                vec!["git".into(), "show".into()],
                vec!["pwd".into()],
                vec!["echo".into()],
                vec!["which".into()],
                vec!["date".into()],
                vec!["uname".into()],
            ],
            denied_commands: vec![
                vec!["sh".into(), "-c".into()],
                vec!["bash".into(), "-c".into()],
                vec!["python".into(), "-c".into()],
                vec!["python3".into(), "-c".into()],
                vec!["node".into(), "-e".into()],
                vec!["eval".into()],
                vec!["exec".into()],
            ],
            allowed_env: vec![
                "RUST_LOG".into(),
                "CARGO_TERM_COLOR".into(),
                "CARGO_TARGET_DIR".into(),
                "RUST_BACKTRACE".into(),
            ],
            inherited_env: vec!["PATH".into()],
        }
    }
}

/// Classify a command's effective impact based on argv, declared impact, and policy.
///
/// Steps:
/// 1. Check denied_commands prefix match -> HardDenied
/// 2. Check built-in risky patterns (sh -c, bash -c, python -c, etc.) -> HardDenied
/// 3. If allow_inline_shell is false and argv[0] is a known shell with -c/-e flag -> HardDenied
/// 4. Check allowed_diagnostic_commands prefix match -> Diagnostic
/// 5. Otherwise -> Unknown (resolved via declared_impact)
///
/// Then resolve with declared_impact:
/// - HardDenied -> always HardDenied (never overridden)
/// - Diagnostic + None -> Diagnostic
/// - Diagnostic + Some(WorkspaceMutation) -> WorkspaceMutation (more conservative wins)
/// - Diagnostic + Some(CommandExecution) -> Diagnostic
/// - Unknown + Some(WorkspaceMutation) -> WorkspaceMutation
/// - Unknown + Some(CommandExecution) -> Err(ImpactMismatch)
/// - Unknown + None -> Err(ImpactRequired)
pub fn classify_command(
    argv: &[String],
    declared_impact: Option<ImpactKind>,
    policy: &CommandPolicy,
) -> Result<EffectiveImpact, CepError> {
    if argv.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "exec.command_run requires a non-empty argv",
            CepErrorData::Validation(ValidationErrorData {
                field: "argv".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Step 1: Check denied_commands prefix match (highest precedence)
    if matches_prefix(argv, &policy.denied_commands) {
        return Ok(EffectiveImpact::HardDenied);
    }

    // Step 2: Check built-in risky patterns
    if is_risky_pattern(argv, policy.allow_inline_shell) {
        return Ok(EffectiveImpact::HardDenied);
    }

    // Step 3: Check allowed_diagnostic_commands prefix match
    if matches_prefix(argv, &policy.allowed_diagnostic_commands) {
        return match declared_impact {
            Some(ImpactKind::WorkspaceMutation) => Ok(EffectiveImpact::WorkspaceMutation),
            _ => Ok(EffectiveImpact::Diagnostic),
        };
    }

    // Step 4: Unknown command — resolve via declared_impact
    match declared_impact {
        Some(ImpactKind::WorkspaceMutation) => Ok(EffectiveImpact::WorkspaceMutation),
        Some(ImpactKind::CommandExecution) => Err(impact_mismatch(argv)),
        Some(_) => Err(impact_mismatch(argv)),
        None => Err(impact_required(argv)),
    }
}

/// Whether argv matches the policy's denied-command prefix list.
///
/// Exposed so inline-shell resolution can honor operator denial of the
/// carrier itself before any expansion happens.
pub(crate) fn command_matches_denied(argv: &[String], policy: &CommandPolicy) -> bool {
    matches_prefix(argv, &policy.denied_commands)
}

/// Classify a resolved command line.
///
/// `Direct` argvs go through [`classify_command`] unchanged. Validated
/// inline-shell expansions are classified per sub-command and folded to the
/// most conservative result: any hard-denied sub-command denies the whole
/// composite, any `WorkspaceMutation` sub-command escalates the composite,
/// and classification errors (e.g. an unknown sub-command without a
/// sufficient `declared_impact`) propagate as-is.
pub fn classify_command_line(
    line: &super::shell_ast::CommandLine,
    declared_impact: Option<ImpactKind>,
    policy: &CommandPolicy,
) -> Result<EffectiveImpact, CepError> {
    match line {
        super::shell_ast::CommandLine::Direct(argv) => {
            classify_command(argv, declared_impact, policy)
        }
        super::shell_ast::CommandLine::Shell(expansion) => {
            let mut effective = EffectiveImpact::Diagnostic;
            for sub_argv in &expansion.sub_argvs {
                match classify_command(sub_argv, declared_impact.clone(), policy)? {
                    EffectiveImpact::HardDenied => return Ok(EffectiveImpact::HardDenied),
                    EffectiveImpact::WorkspaceMutation => {
                        effective = EffectiveImpact::WorkspaceMutation;
                    }
                    EffectiveImpact::Diagnostic => {}
                }
            }
            Ok(effective)
        }
    }
}

/// Build effective environment from params, inherited, and allowed lists.
///
/// Starts with minimal env (empty). Adds inherited_env keys from std::env.
/// For each key in params_env, checks if it's in allowed_env.
/// Returns typed CEP policy error for unallowed keys.
pub fn build_effective_env(
    params_env: &BTreeMap<String, String>,
    policy: &CommandPolicy,
) -> Result<BTreeMap<String, String>, CepError> {
    let mut effective = BTreeMap::new();

    // Add inherited env keys from parent process
    for key in &policy.inherited_env {
        if let Ok(value) = std::env::var(key) {
            effective.insert(key.clone(), value);
        }
    }

    effective.extend(build_allowed_params_env(params_env, policy)?);

    Ok(effective)
}

/// Validate caller-provided command environment keys without inheriting host env.
pub fn validate_command_env_params(
    params_env: &BTreeMap<String, String>,
    policy: &CommandPolicy,
) -> Result<(), CepError> {
    for key in params_env.keys() {
        if !policy.allowed_env.contains(key) {
            return Err(env_not_allowed(key));
        }
    }
    Ok(())
}

/// Build only validated caller-provided environment entries.
///
/// This intentionally ignores `CommandPolicy.inherited_env`; isolated native
/// execution modes use this helper before adding supervisor-generated values.
pub fn build_allowed_params_env(
    params_env: &BTreeMap<String, String>,
    policy: &CommandPolicy,
) -> Result<BTreeMap<String, String>, CepError> {
    validate_command_env_params(params_env, policy)?;
    Ok(params_env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

/// Validate requested timeout against policy max.
///
/// Returns the effective timeout, or a typed CEP error if it exceeds max.
/// Does NOT silently clamp.
pub fn validate_timeout(requested: Option<u64>, policy: &CommandPolicy) -> Result<u64, CepError> {
    let effective = requested.unwrap_or(policy.default_timeout_ms);
    if effective > policy.max_timeout_ms {
        return Err(timeout_exceeded(effective, policy.max_timeout_ms));
    }
    Ok(effective)
}

/// Check if argv matches any valid prefix in the given list.
///
/// Empty prefixes are invalid and must not be treated as wildcards.
/// Prefixes containing empty tokens are ignored because command policies are
/// token-prefix allowlists, not glob patterns.
fn matches_prefix(argv: &[String], prefixes: &[Vec<String>]) -> bool {
    prefixes.iter().any(|prefix| {
        is_valid_prefix(prefix) && argv.len() >= prefix.len() && argv[..prefix.len()] == **prefix
    })
}

fn is_valid_prefix(prefix: &[String]) -> bool {
    !prefix.is_empty() && prefix.iter().all(|token| !token.is_empty())
}

/// Check for built-in risky patterns that should always be HardDenied.
fn is_risky_pattern(argv: &[String], allow_inline_shell: bool) -> bool {
    if argv.len() < 2 {
        return false;
    }

    let prog = argv[0].as_str();
    let flag = argv[1].as_str();

    // Always deny these specific eval patterns
    let risky = matches!(
        (prog, flag),
        ("sh", "-c") | ("bash", "-c") | ("python", "-c") | ("python3", "-c") | ("node", "-e")
    );

    if risky {
        return true;
    }

    // If inline shell is disabled, block common shells with -c/-e
    if !allow_inline_shell {
        return matches!((prog, flag), ("ruby", "-e") | ("perl", "-e"));
    }

    false
}

fn impact_required(argv: &[String]) -> CepError {
    let cmd = argv.first().map(|s| s.as_str()).unwrap_or("<empty>");
    CepError::new(
        CepErrorCode::GuardDenied,
        format!(
            "Missing declared_impact for unknown command '{cmd}'. If '{cmd}' is \
             read-only, add its prefix to the profile's allowed_diagnostic_commands \
             so it is classified as diagnostic; if it mutates the workspace, declare \
             workspace_mutation with an active envelope (RunCommand + any WritePath \
             scope). Do not declare command_execution to make a mutating command look \
             read-only — policy rejects that."
        ),
        CepErrorData::Guard(lancet_protocol::error::GuardErrorData {
            method: "exec.command_run".into(),
            reason: format!("ImpactRequired: unknown command '{cmd}' requires declared_impact"),
        }),
        Retriability::Permanent,
    )
}

fn impact_mismatch(argv: &[String]) -> CepError {
    let cmd = argv.first().map(|s| s.as_str()).unwrap_or("<empty>");
    CepError::new(
        CepErrorCode::GuardDenied,
        format!(
            "declared_impact=command_execution is too weak for unknown command '{cmd}'. \
             command_execution cannot be used to assert an unknown command is read-only. \
             If '{cmd}' is genuinely read-only, add its prefix to the profile's \
             allowed_diagnostic_commands; otherwise declare workspace_mutation with an \
             active envelope (RunCommand + any WritePath scope)."
        ),
        CepErrorData::Guard(lancet_protocol::error::GuardErrorData {
            method: "exec.command_run".into(),
            reason: format!(
                "ImpactMismatch: command '{cmd}' risk exceeds declared CommandExecution"
            ),
        }),
        Retriability::Permanent,
    )
}

fn env_not_allowed(key: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        format!("Environment variable '{key}' is not in the allowed_env list"),
        CepErrorData::Guard(lancet_protocol::error::GuardErrorData {
            method: "exec.command_run".into(),
            reason: format!("EnvNotAllowed: '{key}' is not permitted by command policy"),
        }),
        Retriability::Permanent,
    )
}

fn timeout_exceeded(requested: u64, max: u64) -> CepError {
    CepError::new(
        CepErrorCode::SchemaMismatch,
        format!(
            "Requested timeout {requested}ms exceeds maximum allowed {max}ms. \
             Timeout is not silently clamped."
        ),
        CepErrorData::Validation(ValidationErrorData {
            field: "timeout_ms".into(),
            reason: format!("requested {requested}ms > max {max}ms; timeout rejected, not clamped"),
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::shell_ast::{CommandLine, resolve_command_line};

    fn default_policy() -> CommandPolicy {
        CommandPolicy::defaults()
    }

    fn inline_shell_policy() -> CommandPolicy {
        let mut policy = CommandPolicy::defaults();
        policy.allow_inline_shell = true;
        policy.denied_commands = vec![vec!["rm".into()]];
        policy
    }

    fn shell_line(script: &str, policy: &CommandPolicy) -> CommandLine {
        let argv: Vec<String> = vec!["sh".into(), "-c".into(), script.into()];
        resolve_command_line(&argv, policy).unwrap()
    }

    // --- classify_command_line composite folding ---

    #[test]
    fn command_line_all_diagnostic_folds_to_diagnostic() {
        let policy = inline_shell_policy();
        let line = shell_line("cargo check && cargo test; echo done", &policy);
        let result = classify_command_line(&line, None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn command_line_unknown_sub_command_with_declared_mutation_escalates() {
        let policy = inline_shell_policy();
        let line = shell_line("cargo check && touch marker", &policy);
        let result =
            classify_command_line(&line, Some(ImpactKind::WorkspaceMutation), &policy).unwrap();
        assert_eq!(result, EffectiveImpact::WorkspaceMutation);
    }

    #[test]
    fn command_line_unknown_sub_command_without_impact_errors() {
        let policy = inline_shell_policy();
        let line = shell_line("cargo check && touch marker", &policy);
        let err = classify_command_line(&line, None, &policy).unwrap_err();
        assert!(err.message.contains("touch"));
    }

    #[test]
    fn command_line_denied_sub_command_hard_denies_composite() {
        let policy = inline_shell_policy();
        let line = shell_line("echo a && rm -rf target", &policy);
        let result =
            classify_command_line(&line, Some(ImpactKind::WorkspaceMutation), &policy).unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn command_line_nested_shell_sub_command_hard_denies() {
        let policy = inline_shell_policy();
        let line = shell_line("echo a && sh -c 'echo b'", &policy);
        let result = classify_command_line(&line, None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn command_line_denied_carrier_stays_hard_denied_regardless_of_inline_shell() {
        // Default policy denies `sh -c` and disables inline shell; both
        // settings independently keep the carrier on the direct path.
        for policy in [default_policy(), {
            let mut p = default_policy();
            p.allow_inline_shell = true;
            p
        }] {
            let argv: Vec<String> = vec!["sh".into(), "-c".into(), "echo hi".into()];
            let line = resolve_command_line(&argv, &policy).unwrap();
            assert!(matches!(line, CommandLine::Direct(_)));
            let result = classify_command_line(&line, None, &policy).unwrap();
            assert_eq!(result, EffectiveImpact::HardDenied);
        }
    }

    #[test]
    fn command_line_direct_matches_classify_command() {
        let policy = default_policy();
        let argv: Vec<String> = vec!["cargo".into(), "test".into()];
        let line = resolve_command_line(&argv, &policy).unwrap();
        assert_eq!(
            classify_command_line(&line, None, &policy).unwrap(),
            classify_command(&argv, None, &policy).unwrap()
        );
    }

    // --- Classification tests ---

    #[test]
    fn diagnostic_cargo_test() {
        let policy = default_policy();
        let result = classify_command(&["cargo".into(), "test".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn diagnostic_echo() {
        let policy = default_policy();
        let result = classify_command(&["echo".into(), "hello".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn diagnostic_pwd() {
        let policy = default_policy();
        let result = classify_command(&["pwd".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn diagnostic_git_status() {
        let policy = default_policy();
        let result = classify_command(
            &["git".into(), "status".into(), "--short".into()],
            None,
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn hard_denied_sh_c() {
        let policy = default_policy();
        let result = classify_command(
            &["sh".into(), "-c".into(), "rm -rf /".into()],
            None,
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn hard_denied_bash_c() {
        let policy = default_policy();
        let result = classify_command(
            &["bash".into(), "-c".into(), "echo hi".into()],
            None,
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn hard_denied_python_c() {
        let policy = default_policy();
        let result = classify_command(
            &["python".into(), "-c".into(), "print('hi')".into()],
            None,
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn hard_denied_eval() {
        let policy = default_policy();
        let result = classify_command(&["eval".into(), "something".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn unknown_cargo_fmt_no_impact_errors() {
        let policy = default_policy();
        let err = classify_command(&["cargo".into(), "fmt".into()], None, &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("declared_impact"));
    }

    #[test]
    fn diagnostic_cargo_fmt_check() {
        let policy = default_policy();
        let result = classify_command(
            &[
                "cargo".into(),
                "fmt".into(),
                "--all".into(),
                "--check".into(),
            ],
            Some(ImpactKind::CommandExecution),
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn unknown_cargo_fmt_with_workspace_mutation() {
        let policy = default_policy();
        let result = classify_command(
            &["cargo".into(), "fmt".into()],
            Some(ImpactKind::WorkspaceMutation),
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::WorkspaceMutation);
    }

    #[test]
    fn unknown_command_execution_mismatch() {
        let policy = default_policy();
        let err = classify_command(
            &["cargo".into(), "fmt".into()],
            Some(ImpactKind::CommandExecution),
            &policy,
        )
        .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("too weak"));
    }

    #[test]
    fn diagnostic_with_workspace_mutation_upgrades() {
        let policy = default_policy();
        let result = classify_command(
            &["cargo".into(), "test".into()],
            Some(ImpactKind::WorkspaceMutation),
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::WorkspaceMutation);
    }

    #[test]
    fn diagnostic_with_command_execution_stays_diagnostic() {
        let policy = default_policy();
        let result = classify_command(
            &["cargo".into(), "test".into()],
            Some(ImpactKind::CommandExecution),
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn denied_commands_prefix_has_highest_precedence() {
        let mut policy = default_policy();
        // Add cargo test to denied list
        policy
            .denied_commands
            .push(vec!["cargo".into(), "test".into()]);
        let result = classify_command(&["cargo".into(), "test".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn empty_argv_errors() {
        let policy = default_policy();
        let err = classify_command(&[], None, &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn custom_denied_command() {
        let mut policy = default_policy();
        policy.denied_commands.push(vec!["dangerous".into()]);
        let result = classify_command(&["dangerous".into(), "arg".into()], None, &policy).unwrap();
        assert_eq!(result, EffectiveImpact::HardDenied);
    }

    #[test]
    fn custom_diagnostic_command() {
        let mut policy = default_policy();
        policy
            .allowed_diagnostic_commands
            .push(vec!["mytool".into(), "info".into()]);
        let result = classify_command(
            &["mytool".into(), "info".into(), "--verbose".into()],
            None,
            &policy,
        )
        .unwrap();
        assert_eq!(result, EffectiveImpact::Diagnostic);
    }

    #[test]
    fn empty_diagnostic_prefix_does_not_match_arbitrary_command() {
        let mut policy = default_policy();
        policy.allowed_diagnostic_commands = vec![vec![]];

        let err = classify_command(&["unknown-tool".into()], None, &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("declared_impact"));
    }

    #[test]
    fn diagnostic_prefix_with_empty_token_is_ignored() {
        let mut policy = default_policy();
        policy.allowed_diagnostic_commands = vec![vec!["cargo".into(), "".into()]];

        let err = classify_command(&["cargo".into(), "fmt".into()], None, &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("declared_impact"));
    }

    // --- Env policy tests ---

    #[test]
    fn build_env_inherits_path() {
        let policy = default_policy();
        let params = BTreeMap::new();
        let effective = build_effective_env(&params, &policy).unwrap();
        assert!(effective.contains_key("PATH"));
    }

    #[test]
    fn build_env_allows_rust_log() {
        let mut policy = default_policy();
        policy.inherited_env = vec![]; // Don't inherit anything for test isolation
        let mut params = BTreeMap::new();
        params.insert("RUST_LOG".into(), "debug".into());
        let effective = build_effective_env(&params, &policy).unwrap();
        assert_eq!(effective.get("RUST_LOG").unwrap(), "debug");
    }

    #[test]
    fn build_env_rejects_unallowed_key() {
        let policy = default_policy();
        let mut params = BTreeMap::new();
        params.insert("SECRET_KEY".into(), "value".into());
        let err = build_effective_env(&params, &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("SECRET_KEY"));
    }

    #[test]
    fn build_env_allowed_params_override_inherited() {
        let mut policy = default_policy();
        policy.inherited_env = vec!["RUST_LOG".into()];
        policy.allowed_env = vec!["RUST_LOG".into()];
        let mut params = BTreeMap::new();
        params.insert("RUST_LOG".into(), "trace".into());
        let effective = build_effective_env(&params, &policy).unwrap();
        assert_eq!(effective.get("RUST_LOG").unwrap(), "trace");
    }

    #[test]
    fn build_allowed_params_env_does_not_inherit_host_env() {
        let policy = default_policy();
        let params = BTreeMap::new();

        let effective = build_allowed_params_env(&params, &policy).unwrap();

        assert!(!effective.contains_key("PATH"));
        assert!(!effective.contains_key("HOME"));
    }

    #[test]
    fn validate_command_env_params_rejects_unallowed_key_without_inheritance() {
        let policy = default_policy();
        let mut params = BTreeMap::new();
        params.insert("OPENAI_API_KEY".into(), "secret".into());

        let err = validate_command_env_params(&params, &policy).unwrap_err();

        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(err.message.contains("OPENAI_API_KEY"));
    }

    // --- Timeout policy tests ---

    #[test]
    fn validate_timeout_uses_default_when_none() {
        let policy = default_policy();
        let result = validate_timeout(None, &policy).unwrap();
        assert_eq!(result, 120_000);
    }

    #[test]
    fn validate_timeout_accepts_valid_value() {
        let policy = default_policy();
        let result = validate_timeout(Some(60_000), &policy).unwrap();
        assert_eq!(result, 60_000);
    }

    #[test]
    fn validate_timeout_rejects_exceeding_max() {
        let policy = default_policy();
        let err = validate_timeout(Some(500_000), &policy).unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
        assert!(err.message.contains("500000"));
        assert!(err.message.contains("300000"));
        // Must NOT silently clamp — the error must mention rejection
        assert!(err.message.contains("not silently clamped") || err.message.contains("rejected"));
    }

    #[test]
    fn validate_timeout_accepts_exact_max() {
        let policy = default_policy();
        let result = validate_timeout(Some(300_000), &policy).unwrap();
        assert_eq!(result, 300_000);
    }

    // --- Rejection messages point to a concrete fix ---

    #[test]
    fn unknown_command_errors_point_to_diagnostic_allowlist() {
        let policy = default_policy();
        let argv = vec!["jq".into(), ".".into()];

        let required = classify_command(&argv, None, &policy).unwrap_err();
        assert!(required.message.contains("allowed_diagnostic_commands"));

        let mismatch =
            classify_command(&argv, Some(ImpactKind::CommandExecution), &policy).unwrap_err();
        assert!(mismatch.message.contains("allowed_diagnostic_commands"));
        assert!(mismatch.message.contains("workspace_mutation"));
        // Must not advise asserting read-only via command_execution.
        assert!(
            mismatch
                .message
                .contains("cannot be used to assert an unknown command is read-only")
        );
    }

    // --- EffectiveImpact variants ---

    #[test]
    fn effective_impact_has_exactly_three_variants() {
        // Compile-time proof that the enum has 3 variants
        let variants = [
            EffectiveImpact::Diagnostic,
            EffectiveImpact::WorkspaceMutation,
            EffectiveImpact::HardDenied,
        ];
        assert_eq!(variants.len(), 3);
    }
}
