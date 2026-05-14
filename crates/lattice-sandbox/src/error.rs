#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("path traversal detected: {path}")]
    PathTraversal { path: String },
    #[error("reading '{path}' blocked (matches sensitive pattern '{pattern}')")]
    SensitiveFileRead { path: String, pattern: String },
    #[error("writing '{path}' blocked (matches sensitive pattern '{pattern}')")]
    SensitiveFileWrite { path: String, pattern: String },
    #[error("read from '{path}' not in read allowlist: {allowlist:?}")]
    ReadAllowlist {
        path: String,
        allowlist: Vec<String>,
    },
    #[error("write to '{path}' not in write allowlist: {allowlist:?}")]
    WriteAllowlist {
        path: String,
        allowlist: Vec<String>,
    },
    #[error("command contains dangerous shell metacharacter '{meta}'")]
    Metacharacter { meta: String },
    #[error("command contains ANSI-C quoting which may expand to shell metacharacters")]
    AnsiCQuoting,
    #[error("command contains environment variable override (PATH= or LD_PRELOAD=)")]
    EnvOverride,
    #[error("empty command")]
    EmptyCommand,
    #[error("command '{command}' does not match any allowlist entry. Allowed: {allowed:?}")]
    CommandAllowlist {
        command: String,
        allowed: Vec<String>,
    },
    #[error("command blocked by hook: [{hook}] {reason} (severity={severity})")]
    HookDeny {
        hook: String,
        reason: String,
        severity: String,
    },
    #[error("URL not allowed: {url} ({reason})")]
    UrlDenied { url: String, reason: String },
    #[error("invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}
