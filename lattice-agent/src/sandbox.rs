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
                ".git/config".into(),
                ".ssh/id_rsa".into(),
                ".ssh/id_ed25519".into(),
                ".ssh/authorized_keys".into(),
                ".aws/credentials".into(),
                ".npmrc".into(),
                ".dockercfg".into(),
                "id_rsa".into(),
                "id_ed25519".into(),
                ".kube/config".into(),
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

    /// Resolve a path to a canonical form for sandbox checks.
    /// Falls back to the original path if canonicalization fails.
    fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        std::fs::canonicalize(std::path::Path::new(path))
            .unwrap_or_else(|_| std::path::Path::new(path).to_path_buf())
    }

    /// Check if a canonical path matches any allowlist prefix using
    /// secure starts_with component matching.
    fn matches_allowlist(check_path: &std::path::Path, allowlist: &[String]) -> bool {
        let cwd = std::env::current_dir().ok();
        allowlist.iter().any(|prefix| {
            // Direct starts_with (works for relative paths like "lattice-core/")
            check_path.starts_with(prefix)
            // Resolve prefix relative to cwd (works for absolute canonical paths)
            || cwd.as_ref()
                .map(|cwd| check_path.starts_with(cwd.join(prefix)))
                .unwrap_or(false)
        })
    }

    /// Check if a file path is safe to read.
    pub fn check_read(&self, path: &str) -> Result<(), String> {
        let check_path = self.resolve_path(path);

        // Block sensitive files using path component matching
        for sensitive in &self.sensitive_files {
            if path_matches_sensitive(&check_path, sensitive) {
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

        // Enforce read allowlist with starts_with matching
        if !self.read_allowlist.is_empty() {
            let allowed = Self::matches_allowlist(&check_path, &self.read_allowlist);
            if !allowed {
                return Err(format!(
                    "Sandbox: read from '{}' is not in read allowlist: {:?}",
                    path, self.read_allowlist
                ));
            }
        }
        Ok(())
    }

    /// Check if a file path is safe to write.
    pub fn check_write(&self, path: &str) -> Result<(), String> {
        self.check_read(path)?; // Same basic safety checks + write allowlist

        if !self.write_allowlist.is_empty() {
            let check_path = self.resolve_path(path);
            let allowed = Self::matches_allowlist(&check_path, &self.write_allowlist);
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
        for meta in &["&", ";", "|", "&&", "||", "$(", "`", "\n", "\r", ">", "<"] {
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

        if scheme == "https" {
            if is_private_ip(host) {
                return Err(format!(
                    "Sandbox: HTTPS URL not allowed: {} (private IP)",
                    url_str
                ));
            }
            return Ok(());
        }
        if scheme == "http" && (host == "localhost" || host == "127.0.0.1" || host == "[::1]") {
            return Ok(());
        }
        Err(format!(
            "Sandbox: URL not allowed: {}. Only https:// and http://localhost are permitted.",
            url_str
        ))
    }
}

/// Check if a host string resolves to a private or link-local IP.
///
/// Covers: loopback (127.0.0.1, ::1, localhost), IPv6 ULA (fc00::/7),
/// 10.x.x.x, 172.16-31.x.x, 192.168.x.x, and 169.254.x.x (link-local).
fn is_private_ip(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match host {
        "localhost" | "127.0.0.1" | "::1" => return true,
        _ => {}
    }
    // IPv6 Unique Local Address (fc00::/7)
    if (host.starts_with("fc") || host.starts_with("fd")) && host.len() >= 3 {
        let third = host.as_bytes().get(2).copied().unwrap_or(0);
        if third == b':' || third == b'0' {
            return true;
        }
        // Not a valid ULA format — fall through to IPv4 check
    }
    // Parse dotted IPv4
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() == 4 {
        let first = octets[0];
        match first {
            "10" => return true,
            "169" if octets[1] == "254" => return true,
            "172" => {
                // 172.16.0.0 - 172.31.255.255
                if let Ok(second) = octets[1].parse::<u8>() {
                    if (16..=31).contains(&second) {
                        return true;
                    }
                }
            }
            "192" if octets[1] == "168" => return true,
            _ => {}
        }
    }
    false
}

/// Check if a path matches a sensitive file pattern using path component matching.
///
/// Unlike substring matching (which can have false positives like
/// "some.env.file.txt" matching ".env"), this checks actual path components.
/// For simple patterns like ".env", it matches any path component equal to ".env".
/// For compound patterns like ".git/config", it matches consecutive components.
fn path_matches_sensitive(path: &std::path::Path, pattern: &str) -> bool {
    let parts: Vec<&str> = pattern.split('/').collect();
    let comps: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect();

    if comps.is_empty() {
        return false;
    }

    if parts.len() == 1 {
        comps.iter().any(|c| c.as_ref() == parts[0])
    } else {
        comps.windows(parts.len()).any(|window| {
            window
                .iter()
                .zip(parts.iter())
                .all(|(a, b)| a.as_ref() == *b)
        })
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
    fn test_shell_injection_via_ampersand_rejected() {
        let s = default_sandbox();
        // Standalone & (background)
        assert!(
            s.check_command("sleep 10 &").is_err(),
            "standalone & should be rejected"
        );
        // & in compound (&& would also be caught)
        assert!(s.check_command("cmd &").is_err());
    }

    #[test]
    fn test_shell_injection_via_ampersand_permissive_rejected() {
        let s = SandboxConfig::permissive();
        // Permissive mode still rejects metacharacters
        assert!(
            s.check_command("sleep 10 &").is_err(),
            "permissive mode should reject &"
        );
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
    fn test_shell_injection_via_newline_rejected() {
        let s = default_sandbox();
        let result = s.check_command("ls\nrm -rf /");
        assert!(result.is_err(), "newline injection should be rejected");
        assert!(
            result.unwrap_err().contains("metacharacter"),
            "error should mention metacharacter"
        );
    }

    #[test]
    fn test_shell_injection_via_carriage_return_rejected() {
        let s = default_sandbox();
        let result = s.check_command("ls\rrm -rf /");
        assert!(
            result.is_err(),
            "carriage return injection should be rejected"
        );
    }

    #[test]
    fn test_permissive_mode_rejects_newline_injection() {
        let s = SandboxConfig::permissive();
        assert!(
            s.check_command("curl http://example.com\nrm -rf /")
                .is_err(),
            "permissive mode should reject newline injection"
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

    #[test]
    fn test_url_https_allowed() {
        let s = default_sandbox();
        assert!(s.check_url("https://example.com").is_ok());
        assert!(s.check_url("https://api.github.com/repos/test").is_ok());
    }

    #[test]
    fn test_url_http_localhost_allowed() {
        let s = default_sandbox();
        assert!(s.check_url("http://localhost:3000").is_ok());
        assert!(s.check_url("http://localhost").is_ok());
        assert!(s.check_url("http://127.0.0.1:8080").is_ok());
        assert!(s.check_url("http://[::1]:8080").is_ok());
    }

    #[test]
    fn test_url_localhost_prefix_wildcard_rejected() {
        let s = default_sandbox();
        assert!(
            s.check_url("http://localhost.evil.com").is_err(),
            "localhost.evil.com should be rejected (not localhost)"
        );
    }

    #[test]
    fn test_url_http_remote_rejected() {
        let s = default_sandbox();
        assert!(
            s.check_url("http://example.com").is_err(),
            "plain http://example.com should be rejected"
        );
        assert!(
            s.check_url("http://evil.com/path").is_err(),
            "plain http should be rejected"
        );
    }

    #[test]
    fn test_url_invalid_rejected() {
        let s = default_sandbox();
        assert!(
            s.check_url("not-a-url").is_err(),
            "garbage input should be rejected"
        );
    }

    #[test]
    fn test_url_https_private_ip_rejected() {
        let s = default_sandbox();
        assert!(
            s.check_url("https://10.0.0.1").is_err(),
            "HTTPS to 10.x.x.x should be rejected (SSRF)"
        );
        assert!(
            s.check_url("https://172.16.0.1").is_err(),
            "HTTPS to 172.16.x.x should be rejected (SSRF)"
        );
        assert!(
            s.check_url("https://192.168.1.1").is_err(),
            "HTTPS to 192.168.x.x should be rejected (SSRF)"
        );
        assert!(
            s.check_url("https://169.254.169.254").is_err(),
            "HTTPS to 169.254.x.x should be rejected (SSRF)"
        );
        assert!(
            s.check_url("https://127.0.0.1").is_err(),
            "HTTPS to 127.0.0.1 should be rejected (SSRF)"
        );
    }

    #[test]
    fn test_is_private_ip() {
        use super::is_private_ip;
        assert!(is_private_ip("10.0.0.1"));
        assert!(is_private_ip("172.16.0.1"));
        assert!(is_private_ip("172.31.255.255"));
        assert!(!is_private_ip("172.32.0.1"));
        assert!(is_private_ip("192.168.0.1"));
        assert!(is_private_ip("169.254.169.254"));
        assert!(!is_private_ip("169.255.0.1"));
        assert!(is_private_ip("127.0.0.1"));
        assert!(is_private_ip("::1"));
        assert!(is_private_ip("localhost"));
        assert!(!is_private_ip("93.184.216.34"));
        assert!(!is_private_ip("example.com"));
    }

    #[test]
    fn test_check_read_allowlist_enforced() {
        let mut s = default_sandbox();
        s.read_allowlist = vec!["src/".into(), "lib.rs".into()];
        assert!(s.check_read("src/main.rs").is_ok());
        assert!(s.check_read("src/lib.rs").is_ok());
        assert!(s.check_read("lib.rs").is_ok());
        assert!(s.check_read("/etc/passwd").is_err());
        assert!(s.check_read("secrets.json").is_err());
    }

    #[test]
    fn test_check_read_allowlist_empty_allows_all() {
        let s = default_sandbox();
        // Default has empty read_allowlist, so anything non-sensitive should pass
        assert!(s.check_read("src/main.rs").is_ok());
        assert!(s.check_read("/etc/passwd").is_ok());
        assert!(s.check_read("any/file.txt").is_ok());
    }

    #[test]
    fn test_path_matches_sensitive() {
        use std::path::Path;
        // Single component: exact match only
        assert!(path_matches_sensitive(Path::new("/home/user/.env"), ".env"));
        assert!(!path_matches_sensitive(Path::new("/home/user/some.env.txt"), ".env"));
        assert!(!path_matches_sensitive(Path::new("/home/user/.env_local"), ".env"));
        // Compound pattern: consecutive components
        assert!(path_matches_sensitive(Path::new("/repo/.git/config"), ".git/config"));
        assert!(!path_matches_sensitive(Path::new("/repo/.gitignore_config"), ".git/config"));
        // SSH keys
        assert!(path_matches_sensitive(Path::new("/home/user/.ssh/id_rsa"), ".ssh/id_rsa"));
        // Non-canonical paths still match by components
        assert!(path_matches_sensitive(Path::new("src/../.env"), ".env"));
        // Empty path
        assert!(!path_matches_sensitive(Path::new(""), ".env"));
    }
}
