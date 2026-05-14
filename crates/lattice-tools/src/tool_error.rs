/// Internal error type for tool execution. Not exposed in the ToolExecutor trait
/// signature — errors are converted to String via Display.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ToolError {
    #[error("IO error accessing {path}: {error}")]
    IoError {
        path: String,
        #[source]
        error: std::io::Error,
    },

    #[error("sandbox violation: {0}")]
    SandboxViolation(String),

    #[error("invalid regex pattern: {0}")]
    RegexError(String),

    #[error("HTTP error: {0}")]
    HttpError(String),

    #[error("command error: {0}")]
    CommandError(String),

    #[error("size limit exceeded: {actual} > {limit}")]
    SizeLimit { limit: usize, actual: usize },

    #[error("file not found: {0}")]
    FileNotFound(String),
}
