/// Internal error type for native execution helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ToolError {
    #[error("IO error accessing {path}: {error}")]
    IoError {
        path: String,
        #[source]
        error: std::io::Error,
    },

    #[error("HTTP error: {0}")]
    HttpError(String),

    #[error("size limit exceeded: {actual} > {limit}")]
    SizeLimit { limit: usize, actual: usize },
}
