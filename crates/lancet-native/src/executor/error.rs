// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

/// Internal error type for native execution helpers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ToolError {
    #[allow(dead_code)]
    #[error("IO error accessing {path}: {error}")]
    IoError {
        path: String,
        #[source]
        error: std::io::Error,
    },

    #[cfg(test)]
    #[error("size limit exceeded: {actual} > {limit}")]
    SizeLimit { limit: usize, actual: usize },
}
