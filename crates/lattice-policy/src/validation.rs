/// Validation errors returned when an approval rule fails narrowness checks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    /// Command rule has too few tokens — must be at least 2 (e.g., "cargo test").
    #[error("command rule requires at least {minimum} tokens, got {got}")]
    TooFewTokens { got: usize, minimum: usize },
    /// Command matches a dangerous pattern (rm, sudo, dd, mkfs, chmod 777, rm -rf).
    #[error("command matches dangerous pattern: {pattern}")]
    DangerousPattern { pattern: String },
    /// Path rule has too few depth levels — must be at least 2 (e.g., "/home/user").
    #[error("path rule requires at least {minimum} depth levels, got {got}")]
    PathTooShallow { got: usize, minimum: usize },
    /// Path matches a blocked top-level directory.
    #[error("path matches blocked top-level directory: {path}")]
    BlockedTopLevel { path: String },
    /// The bundle name does not match any known ToolBundle variant.
    #[error("unknown bundle: {0}")]
    UnknownBundle(String),
}

/// Dangerous command patterns that should never be allowed by an approval rule
/// even as part of a longer command (e.g., "sudo apt install" is rejected because
/// "sudo" matches).
pub const DANGEROUS_COMMAND_PATTERNS: &[&str] = &[
    "rm -rf",
    "rm",
    "chmod 777",
    "sudo",
    "dd",
    "mkfs",
    "bash -c",
    "sh -c",
    "zsh -c",
    "fish -c",
    "python -c",
    "python3 -c",
    "node -e",
    "perl -e",
    "ruby -e",
    "php -r",
];

/// Top-level filesystem paths that should never be allowed by an approval rule.
/// Allowing access to `/` or `/etc` would defeat sandbox isolation.
pub const BLOCKED_TOP_LEVEL_PATHS: &[&str] = &[
    "/", "~", "/usr", "/etc", "/bin", "/sbin", "/lib", "/var", "/home", "/root", "/tmp", "/opt",
];

/// Validate an exact command approval rule.
///
/// Exact commands may be single-token because they do not authorize extra
/// arguments. They must still be non-empty and must not match a dangerous
/// command pattern.
pub fn validate_command_exact_rule(command: &str) -> Result<(), ValidationError> {
    let tokens: Vec<&str> = command.split_whitespace().collect();

    if tokens.is_empty() {
        return Err(ValidationError::TooFewTokens { got: 0, minimum: 1 });
    }

    validate_command_is_not_dangerous(command)
}

/// Validate a command prefix approval rule for narrowness.
///
/// A command prefix rule must have at least 2 tokens (e.g., "cargo test") and
/// must not match any dangerous pattern. This prevents overly broad rules like
/// "cargo" or "sudo" from being approved.
pub fn validate_command_prefix_rule(prefix: &str) -> Result<(), ValidationError> {
    let tokens: Vec<&str> = prefix.split_whitespace().collect();

    if tokens.len() < 2 {
        return Err(ValidationError::TooFewTokens {
            got: tokens.len(),
            minimum: 2,
        });
    }

    validate_command_is_not_dangerous(prefix)
}

/// Compatibility alias for existing callers: validates a command prefix rule.
pub fn validate_command_rule(prefix: &str) -> Result<(), ValidationError> {
    validate_command_prefix_rule(prefix)
}

fn validate_command_is_not_dangerous(command: &str) -> Result<(), ValidationError> {
    if let Some(pattern) = dangerous_command_pattern(command) {
        return Err(ValidationError::DangerousPattern {
            pattern: pattern.to_string(),
        });
    }

    Ok(())
}

/// Return the non-approvable dangerous pattern matched by the command prefix.
pub fn dangerous_command_pattern(command: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    DANGEROUS_COMMAND_PATTERNS.iter().copied().find(|pattern| {
        let pattern_tokens: Vec<&str> = pattern.split_whitespace().collect();
        tokens.len() >= pattern_tokens.len()
            && pattern_tokens
                .iter()
                .zip(tokens.iter())
                .all(|(p, t)| p == t)
    })
}

/// Validate a path approval rule for narrowness.
///
/// A path rule must have at least 2 depth levels (e.g., "/home/user") and must
/// not match any blocked top-level directory. This prevents overly broad rules
/// like "/" or "/etc" from being approved.
pub fn validate_path_rule(path: &str) -> Result<(), ValidationError> {
    // Check blocked top-level paths first — "/" has 0 components
    // and "/home" has 1, so depth check would shadow the blocked-path error.
    let trimmed = path.trim_end_matches('/');
    for blocked in BLOCKED_TOP_LEVEL_PATHS {
        if trimmed == *blocked || trimmed == blocked.trim_end_matches('/') {
            return Err(ValidationError::BlockedTopLevel {
                path: blocked.to_string(),
            });
        }
    }

    let components: Vec<&str> = path
        .split('/')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if components.len() < 2 {
        return Err(ValidationError::PathTooShallow {
            got: components.len(),
            minimum: 2,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_command_single_token_rejected() {
        let result = validate_command_rule("cargo");
        assert!(
            matches!(result, Err(ValidationError::TooFewTokens { .. })),
            "single-token rule should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_two_tokens_ok() {
        assert!(validate_command_rule("cargo test").is_ok());
    }

    #[test]
    fn test_validate_command_exact_single_token_ok() {
        assert!(validate_command_exact_rule("make").is_ok());
    }

    #[test]
    fn test_validate_command_exact_empty_rejected() {
        let result = validate_command_exact_rule("   ");
        assert!(
            matches!(
                result,
                Err(ValidationError::TooFewTokens { got: 0, minimum: 1 })
            ),
            "empty exact command should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_dangerous_rm() {
        let result = validate_command_rule("rm -rf /tmp");
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "rm -rf should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_dangerous_sudo() {
        let result = validate_command_rule("sudo apt install");
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "sudo should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_dangerous_dd() {
        let result = validate_command_rule("dd if=/dev/zero");
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "dd should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_dangerous_chmod_777() {
        let result = validate_command_rule("chmod 777 /tmp");
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "chmod 777 should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_command_rejects_shell_escape_interpreters() {
        for command in [
            "bash -c echo hi",
            "sh -c echo hi",
            "python -c print(1)",
            "node -e console.log(1)",
        ] {
            let result = validate_command_rule(command);
            assert!(
                matches!(result, Err(ValidationError::DangerousPattern { .. })),
                "{command} should be rejected: {:?}",
                result
            );
        }
    }

    #[test]
    fn test_validate_command_exact_rejects_dangerous_single_token() {
        let result = validate_command_exact_rule("rm");
        assert!(
            matches!(result, Err(ValidationError::DangerousPattern { .. })),
            "exact rm command should be rejected: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_path_single_level_rejected() {
        let result = validate_path_rule("/usr");
        assert!(result.is_err(), "/usr should be rejected: {:?}", result);
    }

    #[test]
    fn test_validate_path_two_levels_ok() {
        assert!(validate_path_rule("/home/user/project").is_ok());
    }

    #[test]
    fn test_validate_path_blocked_root() {
        let result = validate_path_rule("/");
        assert!(
            matches!(result, Err(ValidationError::BlockedTopLevel { .. })),
            "/ should be blocked: {:?}",
            result
        );
    }

    #[test]
    fn test_validate_path_blocked_home() {
        let result = validate_path_rule("/home");
        assert!(
            matches!(result, Err(ValidationError::BlockedTopLevel { .. })),
            "/home should be blocked: {:?}",
            result
        );
    }
}
