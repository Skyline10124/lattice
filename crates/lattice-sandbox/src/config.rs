use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::approval::ApprovedRule;
use crate::audit::AuditSink;
use crate::hooks::CommandHook;
pub use lattice_policy::PolicyAuthority;
pub use lattice_policy::ToolCatalog;

/// Landlock file isolation configuration for Linux (kernel >= 5.13).
///
/// When enabled, bash subprocesses are restricted with Landlock LSM:
/// - System paths get read-only + execute access (via `/` rule)
/// - Project root and `/tmp` get full read-write access
/// - All other paths are denied
///
/// This is layered on top of [`SandboxConfig`] path allowlist checks,
/// providing defense-in-depth at the kernel level.
///
/// Set to `None` to disable Landlock (default). On non-Linux platforms
/// or kernels < 5.13, Landlock is silently skipped.
#[derive(Clone, Default)]
pub struct LandlockConfig {
    /// Additional read paths beyond the default system paths.
    pub extra_read_paths: Vec<PathBuf>,
    /// Additional write paths beyond project_root and /tmp.
    pub extra_write_paths: Vec<PathBuf>,
}

/// Sandbox configuration for tool execution safety.
///
/// Controls which paths can be read/written, which commands can run,
/// which files are blocked, and size/timeout limits for all tool
/// operations executed by `DefaultToolExecutor`.
#[derive(Clone)]
pub struct SandboxConfig {
    /// Optional CommandHook for pre-execute security checks (e.g., Tirith).
    /// When set, `check_command` runs the hook BEFORE metacharacter checks,
    /// so Tirith can detect patterns like `curl | sh` before `|` is blocked.
    pub hook: Option<Arc<dyn CommandHook>>,
    /// Landlock file isolation config (Linux only, kernel >= 5.13).
    /// When `Some`, bash subprocesses get kernel-level file isolation
    /// in addition to the userspace path allowlist checks.
    /// Defaults to `None` (Landlock disabled).
    pub landlock: Option<LandlockConfig>,
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
    /// Maximum HTTP response size for web_search (bytes). Default: 10 MB.
    pub max_http_response_size: usize,
    /// Commands allowed via bash/run_command. Empty = any command.
    pub command_allowlist: Vec<String>,
    /// Dynamic approval rules that supplement the static allowlists.
    /// Shared with `RuntimeSecurity` via `Arc<Mutex>` so approved rules
    /// from the CLI/runtime layer are visible to sandbox checks.
    /// `None` when no approval system is active (backward compatible).
    pub approved_rules: Option<Arc<Mutex<Vec<ApprovedRule>>>>,
    /// Policy authority for centralized dynamic approval decisions.
    /// When set, sandbox checks delegate path/command/tool decisions to
    /// this authority instead of matching against `approved_rules` inline.
    /// When absent, falls back to the `approved_rules` field for backward
    /// compatibility.
    pub policy_authority: Option<PolicyAuthority>,
    /// Max command execution time (seconds). Default: 30.
    pub max_command_timeout: u32,
    /// Optional audit sink for security event recording.
    /// When set, `check_command` emits ToolExec and HookDeny events.
    pub audit: Option<Arc<dyn AuditSink>>,
    /// Human-readable label for audit logging (e.g., "strict", "permissive").
    pub sandbox_label: String,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            // No hook by default (backward compatible).
            hook: None,
            // Landlock disabled by default.
            landlock: None,
            // Empty allowlists mean "no restriction" (any path passes).
            // For restrictive defaults within a project, use `SandboxConfig::project_dirs()`.
            read_allowlist: vec![],
            write_allowlist: vec![],
            sensitive_files: vec![
                // .env variants
                ".env".into(),
                ".env.local".into(),
                ".env.development".into(),
                ".env.production".into(),
                ".env.staging".into(),
                ".env.test".into(),
                ".env.*".into(), // wildcard: matches .env.anything
                // Cloud credentials
                "credentials.json".into(),
                ".aws/credentials".into(),
                ".aws/config".into(),
                ".kube/config".into(),
                ".gcp/service-account.json".into(),
                ".azure/credentials".into(),
                // SSH keys and config
                ".ssh/id_rsa".into(),
                ".ssh/id_ed25519".into(),
                ".ssh/id_ecdsa".into(),
                ".ssh/id_*".into(), // wildcard: matches id_rsa, id_ed25519, id_ecdsa, etc.
                ".ssh/authorized_keys".into(),
                ".ssh/config".into(),
                // PGP/GPG
                ".gnupg".into(),
                // Package manager tokens
                ".npmrc".into(),
                ".dockercfg".into(),
                ".pypirc".into(),
                ".gem/credentials".into(),
                ".netrc".into(),
                // Git
                ".git/config".into(),
                ".git/credentials".into(),
                // TLS/crypto files (wildcards)
                "*.pem".into(),
                "*.key".into(),
                "*.p12".into(),
                "*.pfx".into(),
                // Generic
                "secrets".into(),
                "id_rsa".into(),
                "id_ed25519".into(),
            ],
            max_read_size: 10 * 1024 * 1024,          // 10 MB
            max_write_size: 1024 * 1024,              // 1 MB
            max_http_response_size: 10 * 1024 * 1024, // 10 MB
            command_allowlist: vec!["grep".into(), "find".into(), "ls".into(), "ps".into()],
            approved_rules: None,
            policy_authority: None,
            max_command_timeout: 30,
            audit: None,
            sandbox_label: "strict".into(),
        }
    }
}

impl SandboxConfig {
    /// Permissive mode: no path/command restrictions beyond metacharacter and
    /// sensitive-file blocking. Suitable for trusted development environments.
    pub fn permissive() -> Self {
        Self {
            read_allowlist: vec![],
            write_allowlist: vec![],
            command_allowlist: vec![],
            sandbox_label: "permissive".into(),
            ..Default::default()
        }
    }

    /// Restrict writes and commands to a specific set of project directories.
    /// Use this when running in a known project layout where the agent should
    /// only modify designated source dirs and run designated dev commands.
    pub fn project_dirs(write_dirs: Vec<String>, dev_commands: Vec<String>) -> Self {
        Self {
            write_allowlist: write_dirs,
            command_allowlist: dev_commands,
            ..Default::default()
        }
    }

    /// Resolve a path to a canonical form for sandbox checks.
    /// Falls back to the original path if canonicalization fails.
    pub(crate) fn resolve_path(&self, path: &str) -> std::path::PathBuf {
        std::fs::canonicalize(std::path::Path::new(path))
            .unwrap_or_else(|_| std::path::Path::new(path).to_path_buf())
    }

    /// Check if a canonical path matches any allowlist prefix using
    /// component-boundary matching. This prevents byte-prefix attacks
    /// (e.g., "/home/user/src-hack/" matching allowlist entry "/home/user/src").
    pub(crate) fn matches_allowlist(check_path: &std::path::Path, allowlist: &[String]) -> bool {
        use crate::path_utils::is_path_prefix;

        let cwd = std::env::current_dir().ok();
        allowlist.iter().any(|prefix| {
            let prefix_path = std::path::Path::new(prefix);
            // Exact match
            if check_path == prefix_path {
                return true;
            }
            // Component-boundary prefix: check_path must start with prefix AND
            // the next character after the prefix is a separator (or check_path
            // equals the prefix exactly — handled above). This prevents
            // "/home/user/src-hack/" from matching "/home/user/src".
            if is_path_prefix(check_path, prefix_path) {
                return true;
            }
            // Try resolving prefix relative to cwd
            if let Some(ref cwd) = cwd {
                let full = cwd.join(prefix);
                if check_path == full || is_path_prefix(check_path, &full) {
                    return true;
                }
            }
            false
        })
    }
}
