// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Error type for the LANCET public API.

/// Errors surfaced while constructing or driving a LANCET runtime.
///
/// Callers map these onto their own error vocabulary: configuration problems are
/// caller-facing (bad request) while [`LancetError::Internal`] is an unexpected
/// runtime fault.
#[derive(Debug, thiserror::Error)]
pub enum LancetError {
    /// 运行时配置无效，例如路径、工作区历史绑定或输入信封不合法。
    #[error("invalid lancet configuration: {0}")]
    Config(String),

    /// An unexpected internal fault while assembling the runtime.
    #[error("{0}")]
    Internal(String),

    /// The enabled tool list referenced names absent from the production catalog.
    #[error("unknown enabled tools {unknown:?}; available catalog tools: {available:?}")]
    UnknownTools {
        /// Requested tool names not present in the catalog.
        unknown: Vec<String>,
        /// Tool names available in the catalog.
        available: Vec<String>,
    },
}

/// Convenience result alias for the LANCET public API.
pub type LancetResult<T> = Result<T, LancetError>;
