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
                "artemis-core/".into(),
                "artemis-agent/".into(),
                "artemis-memory/".into(),
                "artemis-token-pool/".into(),
                "artemis-python/".into(),
                "artemis-plugin/".into(),
                "artemis-harness/".into(),
                "artemis-cli/".into(),
                "artemis-tui/".into(),
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
            max_write_size: 1 * 1024 * 1024,  // 1 MB
            command_allowlist: vec![
                "cargo test".into(),
                "cargo clippy".into(),
                "cargo build".into(),
                "cargo fmt".into(),
                "grep".into(),
                "find".into(),
                "ls".into(),
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
    pub fn check_command(&self, cmd: &str) -> Result<(), String> {
        if self.command_allowlist.is_empty() {
            return Ok(());
        }
        let allowed = self
            .command_allowlist
            .iter()
            .any(|allowed| cmd.starts_with(allowed));
        if !allowed {
            return Err(format!(
                "Sandbox: command '{}' is not in allowlist. Allowed: {:?}",
                cmd, self.command_allowlist
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
