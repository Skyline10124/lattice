use crate::approval::ApprovalKind;
use crate::audit::AuditEventKind;
use crate::command_utils::command_matches_allowlist;
use crate::config::SandboxConfig;
use crate::denial::{DenialKind, DenialReport, RiskClass};
use crate::path_utils::path_matches_sensitive;
use crate::security::reject_private_resolved_host;
use crate::PathOperation;
use lattice_policy::{
    command_matches_exact, path_matches_exact, path_matches_recursive, PolicyDecision,
};

impl SandboxConfig {
    /// Check if a file path is safe to read.
    pub fn check_read(&self, path: &str) -> Result<(), String> {
        self.check_read_typed(path).map_err(|d| d.to_json())
    }

    /// Typed variant of [`check_read`] returning a structured `DenialReport`
    /// instead of a plain string on failure.
    ///
    /// The report includes the denied path, operation, risk class, and the
    /// current allowlist entries so the agent loop can suggest targeted
    /// approval rules (per D-08, D-09).
    #[allow(clippy::result_large_err)]
    pub fn check_read_typed(&self, path: &str) -> Result<(), DenialReport> {
        let check_path = self.resolve_path(path);

        // Block path traversal
        if path.contains("..") {
            return Err(DenialReport::new(
                format!("Sandbox: path '{}' contains '..'", path),
                RiskClass::Read,
                DenialKind::Path {
                    path: path.to_string(),
                    operation: "read".into(),
                    allowlist: self.read_allowlist.clone(),
                },
            ));
        }

        // Block sensitive files using path component matching
        for sensitive in &self.sensitive_files {
            if path_matches_sensitive(&check_path, sensitive) {
                return Err(DenialReport::new(
                    format!(
                        "Sandbox: reading '{}' is blocked (matches sensitive pattern '{}')",
                        path, sensitive
                    ),
                    RiskClass::Read,
                    DenialKind::Path {
                        path: path.to_string(),
                        operation: "read".into(),
                        allowlist: self.read_allowlist.clone(),
                    },
                ));
            }
        }

        // Enforce read allowlist with component-boundary matching
        if !self.read_allowlist.is_empty() {
            if let Some(ref authority) = self.policy_authority {
                match authority.decide_path(&check_path, PathOperation::Read, &self.read_allowlist)
                {
                    PolicyDecision::Allow => {}
                    PolicyDecision::Deny(report) => return Err(report),
                }
            } else {
                let allowed = Self::matches_allowlist(&check_path, &self.read_allowlist);
                if !allowed {
                    // Check approved_rules
                    if let Some(ref rules) = self.approved_rules {
                        let rules = rules.lock().unwrap_or_else(|e| e.into_inner());
                        let dynamically_allowed = rules.iter().any(|rule| match &rule.kind {
                            ApprovalKind::PathExact {
                                path,
                                operation: PathOperation::Read,
                            } => path_matches_exact(&check_path, path),
                            ApprovalKind::PathRecursive {
                                root,
                                operation: PathOperation::Read,
                            } => path_matches_recursive(&check_path, root),
                            _ => false,
                        });
                        if dynamically_allowed {
                            return Ok(());
                        }
                    }

                    return Err(DenialReport::new(
                        format!(
                            "Sandbox: read from '{}' is not in read allowlist: {:?}",
                            path, self.read_allowlist
                        ),
                        RiskClass::Read,
                        DenialKind::Path {
                            path: path.to_string(),
                            operation: "read".into(),
                            allowlist: self.read_allowlist.clone(),
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    /// Check if a file path is safe to write.
    pub fn check_write(&self, path: &str) -> Result<(), String> {
        self.check_write_typed(path).map_err(|d| d.to_json())
    }

    /// Typed variant of [`check_write`] returning a structured `DenialReport`
    /// instead of a plain string on failure.
    #[allow(clippy::result_large_err)]
    pub fn check_write_typed(&self, path: &str) -> Result<(), DenialReport> {
        let check_path = self.resolve_path(path);

        // Block path traversal
        if path.contains("..") {
            return Err(DenialReport::new(
                format!("Sandbox: path '{}' contains '..'", path),
                RiskClass::Mutate,
                DenialKind::Path {
                    path: path.to_string(),
                    operation: "write".into(),
                    allowlist: self.write_allowlist.clone(),
                },
            ));
        }

        // Block sensitive files
        for sensitive in &self.sensitive_files {
            if path_matches_sensitive(&check_path, sensitive) {
                return Err(DenialReport::new(
                    format!(
                        "Sandbox: writing '{}' is blocked (matches sensitive pattern '{}')",
                        path, sensitive
                    ),
                    RiskClass::Mutate,
                    DenialKind::Path {
                        path: path.to_string(),
                        operation: "write".into(),
                        allowlist: self.write_allowlist.clone(),
                    },
                ));
            }
        }

        // Enforce write allowlist with component-boundary matching
        if !self.write_allowlist.is_empty() {
            if let Some(ref authority) = self.policy_authority {
                match authority.decide_path(
                    &check_path,
                    PathOperation::Write,
                    &self.write_allowlist,
                ) {
                    PolicyDecision::Allow => {}
                    PolicyDecision::Deny(report) => return Err(report),
                }
            } else {
                let allowed = Self::matches_allowlist(&check_path, &self.write_allowlist);
                if !allowed {
                    // Check approved_rules
                    if let Some(ref rules) = self.approved_rules {
                        let rules = rules.lock().unwrap_or_else(|e| e.into_inner());
                        let dynamically_allowed = rules.iter().any(|rule| match &rule.kind {
                            ApprovalKind::PathExact {
                                path,
                                operation: PathOperation::Write,
                            } => path_matches_exact(&check_path, path),
                            ApprovalKind::PathRecursive {
                                root,
                                operation: PathOperation::Write,
                            } => path_matches_recursive(&check_path, root),
                            _ => false,
                        });
                        if dynamically_allowed {
                            return Ok(());
                        }
                    }

                    return Err(DenialReport::new(
                        format!(
                            "Sandbox: write to '{}' is not in write allowlist: {:?}",
                            path, self.write_allowlist
                        ),
                        RiskClass::Mutate,
                        DenialKind::Path {
                            path: path.to_string(),
                            operation: "write".into(),
                            allowlist: self.write_allowlist.clone(),
                        },
                    ));
                }
            }
        }
        Ok(())
    }

    /// Check if a command is safe to run.
    ///
    /// Flow: CommandHook (optional) -> metacharacter check -> command allowlist.
    /// CommandHook runs FIRST so semantic patterns (e.g., `curl | sh`) are detected
    /// before the metacharacter `|` is rejected generically.
    ///
    /// Uses token-prefix allowlist matching: single-word entries match by program
    /// name, multi-word entries match by program + subcommand prefix. Also rejects
    /// dangerous shell metacharacters that enable command injection via `sh -c`
    /// execution.
    pub fn check_command(&self, cmd: &str) -> Result<(), String> {
        let cmd = cmd.trim();

        // Phase 1: CommandHook (optional) — semantic security checks BEFORE
        // metacharacter filtering. This lets Tirith detect patterns like
        // `curl | sh` before `|` is rejected generically.
        let cmd = if let Some(ref hook) = self.hook {
            match hook.run_pre_execute("bash", cmd) {
                Ok(modified) => modified,
                Err(reason) => {
                    // Audit: log the hook deny reason.
                    if let Some(ref audit) = self.audit {
                        audit.log_sync(AuditEventKind::HookDeny {
                            hook: reason.hook.clone(),
                            tool: "bash".into(),
                            command: cmd.to_string(),
                            reason: reason.reason.clone(),
                            severity: reason.severity.to_string(),
                        });
                    }
                    return Err(format!(
                        "Sandbox: command blocked by HookChain: [{}] {} (severity={})",
                        reason.hook, reason.reason, reason.severity
                    ));
                }
            }
        } else {
            cmd.to_string()
        };

        // Phase 2: metacharacter, env-override checks.
        // These are security-critical and NOT approvable — they run BEFORE
        // the allowlist/approved_rules phase per D-19.
        self.check_metacharacters(&cmd)?;

        // Phase 3: allowlist check via typed method (consults approved_rules).
        let result = self.check_command_typed(&cmd);

        // Audit: log tool_exec outcome (ok / denied) for non-hook paths.
        if let Some(ref audit) = self.audit {
            let outcome = if result.is_ok() { "ok" } else { "denied" };
            audit.log_sync(AuditEventKind::ToolExec {
                tool: "bash".into(),
                command: cmd.to_string(),
                label: self.sandbox_label.clone(),
                outcome: outcome.to_string(),
            });
        }

        result.map_err(|d| d.to_json())
    }

    /// Metacharacter and environment-override checks that cannot be bypassed
    /// by approved rules. Returns `Err(String)` for non-approvable denials.
    fn check_metacharacters(&self, cmd: &str) -> Result<(), String> {
        // Reject shell metacharacters that enable command injection.
        if cmd.contains("$'") {
            return Err(
                "Sandbox: command contains ANSI-C quoting ($'...') which may expand to shell metacharacters"
                    .into(),
            );
        }

        // Reject PATH= or LD_PRELOAD= environment variable overrides
        let upper = cmd.to_uppercase();
        if upper.contains("PATH=") || upper.contains("LD_PRELOAD=") {
            return Err(
                "Sandbox: command contains environment variable override (PATH= or LD_PRELOAD=) which is not allowed"
                    .into(),
            );
        }

        for meta in &["&", ";", "|", "&&", "||", "$(", "`", "\n", "\r", ">", "<"] {
            if cmd.contains(meta) {
                return Err(format!(
                    "Sandbox: command contains dangerous shell metacharacter '{}'",
                    meta
                ));
            }
        }

        Ok(())
    }

    /// Allowlist-only check (called after metacharacter checks pass).
    ///
    /// Checks the static `command_allowlist` first, then consults
    /// `approved_rules` or `policy_authority` for dynamically-approved
    /// commands. Returns a structured `DenialReport` on failure so the
    /// agent loop can detect denials and suggest approval rules.
    #[allow(clippy::result_large_err)]
    pub fn check_command_typed(&self, cmd: &str) -> Result<(), DenialReport> {
        if self.command_allowlist.is_empty() {
            return Ok(());
        }

        if cmd.split_whitespace().next().unwrap_or("").is_empty() {
            return Err(DenialReport::new(
                "Sandbox: empty command".into(),
                RiskClass::Mutate,
                DenialKind::Command {
                    command: cmd.to_string(),
                    allowed_prefixes: self.command_allowlist.clone(),
                },
            ));
        }

        if let Some(ref authority) = self.policy_authority {
            match authority.decide_command(cmd, &self.command_allowlist) {
                PolicyDecision::Allow => return Ok(()),
                PolicyDecision::Deny(report) => return Err(report),
            }
        }

        let allowed = self
            .command_allowlist
            .iter()
            .any(|entry| command_matches_allowlist(cmd, entry));

        if allowed {
            return Ok(());
        }

        // Check approved_rules (dynamically-approved exact commands/prefixes).
        // D-19: re-validate the actual command against dangerous command patterns
        // before allowing. Even if a rule was manually inserted bypassing
        // validate_command_rule, dangerous commands must still be blocked.
        if let Some(ref rules) = self.approved_rules {
            let rules = rules.lock().unwrap_or_else(|e| e.into_inner());
            let dynamically_allowed = rules.iter().any(|rule| {
                let is_dangerous = lattice_policy::dangerous_command_pattern(cmd).is_some();
                if is_dangerous {
                    return false;
                }

                match &rule.kind {
                    ApprovalKind::CommandExact { command } => command_matches_exact(cmd, command),
                    ApprovalKind::CommandPrefix { prefix } => {
                        command_matches_allowlist(cmd, prefix)
                    }
                    _ => false,
                }
            });
            if dynamically_allowed {
                return Ok(());
            }
        }

        let allowed_prefixes: Vec<String> = self.command_allowlist.clone();
        Err(DenialReport::new(
            format!(
                "Sandbox: command '{}' does not match any allowlist entry. Allowed: {:?}",
                cmd.trim(),
                allowed_prefixes
                    .iter()
                    .map(|e| e.as_ref())
                    .collect::<Vec<&str>>()
            ),
            RiskClass::Mutate,
            DenialKind::Command {
                command: cmd.trim().to_string(),
                allowed_prefixes,
            },
        ))
    }

    /// Check if a URL scheme is safe.
    ///
    /// Rules:
    /// - HTTPS allowed only to non-private IPs (SSRF protection).
    /// - HTTP allowed only to explicit localhost/127.0.0.1/[::1].
    /// - Private IPs (10.x, 172.16-31.x, 192.168.x, 169.254.x, loopback) are
    ///   rejected for all schemes except the http localhost allowlist above.
    pub fn check_url(&self, url_str: &str) -> Result<(), String> {
        let parsed = url::Url::parse(url_str).map_err(|e| format!("invalid URL: {}", e))?;
        let scheme = parsed.scheme();
        let host = parsed.host_str().unwrap_or("");

        // Block known DNS rebinding service domains that can resolve to internal IPs
        let rebinding_domains = ["nip.io", "xip.io"];
        if rebinding_domains
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{}", d)))
        {
            return Err(format!(
                "Sandbox: URL not allowed: {} (DNS rebinding domain '{}' blocked)",
                url_str, host
            ));
        }

        if scheme == "https" {
            let port = parsed.port_or_known_default().unwrap_or(443);
            if let Err(reason) = reject_private_resolved_host(host, port) {
                return Err(format!(
                    "Sandbox: HTTPS URL not allowed: {} ({})",
                    url_str, reason
                ));
            }
            return Ok(());
        }
        if scheme == "http" && matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") {
            return Ok(());
        }
        Err(format!(
            "Sandbox: URL not allowed: {}. Only https:// and http://localhost are permitted.",
            url_str
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ApprovedRule;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn config_with_approved_rules(rules: Vec<ApprovedRule>) -> SandboxConfig {
        let mut config = SandboxConfig::default();
        config.approved_rules = Some(Arc::new(Mutex::new(rules)));
        config
    }

    fn config_with_policy_authority(rules: Vec<ApprovedRule>) -> SandboxConfig {
        let shared = Arc::new(Mutex::new(rules));
        let mut config = SandboxConfig::default();
        config.approved_rules = Some(Arc::clone(&shared));
        config.policy_authority = Some(crate::PolicyAuthority::from_shared_rules(shared));
        config
    }

    #[test]
    fn test_check_command_returns_denial_json() {
        // Default SandboxConfig has a restricted command_allowlist (grep, find, ls, ps)
        let config = SandboxConfig::default();
        let result = config.check_command("cargo publish");
        assert!(result.is_err(), "cargo publish should be denied");
        let err = result.unwrap_err();
        assert!(
            err.contains("\"__denial_report__\":true"),
            "denial should be DenialReport JSON containing sentinel: {}",
            err
        );
        assert!(
            err.contains("\"type\":\"command\""),
            "denial should include command kind tag: {}",
            err
        );
    }

    #[test]
    fn test_approved_command_rule_allows() {
        let config = config_with_approved_rules(vec![ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo publish".to_string(),
            },
            risk_class: RiskClass::Mutate,
        }]);
        // "cargo publish --dry-run" should match the approved prefix
        assert!(
            config.check_command("cargo publish --dry-run").is_ok(),
            "approved command rule should allow matching command"
        );
    }

    #[test]
    fn test_approved_command_exact_allows_only_exact_command() {
        let config = config_with_approved_rules(vec![ApprovedRule {
            kind: ApprovalKind::CommandExact {
                command: "cargo publish".to_string(),
            },
            risk_class: RiskClass::Mutate,
        }]);

        assert!(
            config.check_command("cargo publish").is_ok(),
            "exact command approval should allow the literal command"
        );
        assert!(
            config.check_command("cargo publish --dry-run").is_err(),
            "exact command approval must not allow additional args"
        );
    }

    #[test]
    fn test_policy_authority_command_exact_allows_only_exact_string() {
        let config = config_with_policy_authority(vec![ApprovedRule {
            kind: ApprovalKind::CommandExact {
                command: "cargo publish".to_string(),
            },
            risk_class: RiskClass::Mutate,
        }]);

        assert!(
            config.check_command("cargo publish").is_ok(),
            "policy authority exact command should allow the literal command"
        );
        assert!(
            config.check_command("cargo   publish").is_err(),
            "policy authority exact command must not normalize internal whitespace"
        );
    }

    #[test]
    fn test_approved_rule_cannot_bypass_metachar() {
        let config = config_with_approved_rules(vec![ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "cargo publish".to_string(),
            },
            risk_class: RiskClass::Mutate,
        }]);
        // Command with pipe metacharacter should still be rejected
        let result = config.check_command("cargo publish | sh");
        assert!(result.is_err(), "metacharacter should still be blocked");
        let err = result.unwrap_err();
        assert!(
            err.contains("dangerous shell metacharacter"),
            "should report metacharacter denial: {}",
            err
        );
    }

    #[test]
    fn test_approved_rule_cannot_bypass_interpreter_escape() {
        let config = config_with_approved_rules(vec![ApprovedRule {
            kind: ApprovalKind::CommandPrefix {
                prefix: "bash -c".to_string(),
            },
            risk_class: RiskClass::Mutate,
        }]);

        let result = config.check_command("bash -c echo");
        assert!(
            result.is_err(),
            "interpreter escape command should still be blocked"
        );
        let err = result.unwrap_err();
        assert!(
            err.contains("\"__denial_report__\":true"),
            "interpreter denial should remain structured: {}",
            err
        );
    }

    #[test]
    fn test_check_read_returns_denial_json() {
        let mut config = SandboxConfig::default();
        config.read_allowlist = vec!["/home/user/project".to_string()];
        // Try reading outside the allowlist
        let result = config.check_read("/etc/passwd");
        assert!(result.is_err(), "/etc/passwd should be denied");
        let err = result.unwrap_err();
        assert!(
            err.contains("\"__denial_report__\":true"),
            "denial should be DenialReport JSON containing sentinel: {}",
            err
        );
        assert!(
            err.contains("\"type\":\"path\""),
            "denial should include path kind tag: {}",
            err
        );
    }

    #[test]
    fn test_approved_path_rule_allows_read() {
        let mut config = SandboxConfig::default();
        // Restrictive allowlist that does NOT cover the target path
        config.read_allowlist = vec!["/restricted/allowed-dir".to_string()];
        config.approved_rules = Some(Arc::new(Mutex::new(vec![ApprovedRule {
            kind: ApprovalKind::PathRecursive {
                root: PathBuf::from("/restricted/approved-extra"),
                operation: PathOperation::Read,
            },
            risk_class: RiskClass::Read,
        }])));
        // Static allowlist "/restricted/allowed-dir" does NOT match
        // "/restricted/approved-extra/file.txt", but the approved rule
        // for "/restricted/approved-extra" DOES match as a path prefix.
        let result = config.check_read("/restricted/approved-extra/file.txt");
        assert!(
            result.is_ok(),
            "approved path rule should allow read: {:?}",
            result
        );
    }

    #[test]
    fn test_policy_authority_path_rule_allows_read() {
        let mut config = config_with_policy_authority(vec![ApprovedRule {
            kind: ApprovalKind::PathRecursive {
                root: PathBuf::from("/restricted/approved-extra"),
                operation: PathOperation::Read,
            },
            risk_class: RiskClass::Read,
        }]);
        config.read_allowlist = vec!["/restricted/allowed-dir".to_string()];

        let result = config.check_read("/restricted/approved-extra/file.txt");
        assert!(
            result.is_ok(),
            "policy authority path rule should allow read: {:?}",
            result
        );
    }

    #[test]
    fn test_approved_path_exact_allows_only_exact_path() {
        let mut config = SandboxConfig::default();
        config.read_allowlist = vec!["/restricted/allowed-dir".to_string()];
        config.approved_rules = Some(Arc::new(Mutex::new(vec![ApprovedRule {
            kind: ApprovalKind::PathExact {
                path: PathBuf::from("/restricted/approved-extra/file.txt"),
                operation: PathOperation::Read,
            },
            risk_class: RiskClass::Read,
        }])));

        assert!(
            config
                .check_read("/restricted/approved-extra/file.txt")
                .is_ok(),
            "exact path approval should allow the literal path"
        );
        assert!(
            config
                .check_read("/restricted/approved-extra/file.txt.bak")
                .is_err(),
            "exact path approval must not allow sibling or child-like paths"
        );
    }

    #[test]
    fn test_approved_path_rule_uses_component_boundary() {
        let mut config = SandboxConfig::default();
        config.read_allowlist = vec!["/restricted/allowed-dir".to_string()];
        config.approved_rules = Some(Arc::new(Mutex::new(vec![ApprovedRule {
            kind: ApprovalKind::PathRecursive {
                root: PathBuf::from("/restricted/approved"),
                operation: PathOperation::Read,
            },
            risk_class: RiskClass::Read,
        }])));

        let result = config.check_read("/restricted/approved-extra/file.txt");
        assert!(
            result.is_err(),
            "approved path prefix must not match sibling paths"
        );
    }
}
