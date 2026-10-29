// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub(super) fn store(home: Option<&std::path::Path>) -> super::Result<lattice_llm::auth::AuthStore> {
    Ok(lattice_llm::auth::AuthStore::new(directory(home)?))
}

pub(super) fn directory(home: Option<&std::path::Path>) -> super::Result<std::path::PathBuf> {
    let reason = match home {
        Some(home) if home.is_absolute() => {
            return Ok(home.join(".lattice"));
        }
        Some(_) => "HOME must be an absolute path",
        None => "HOME is not set",
    };
    Err(crate::ApiError::Configuration(crate::ConfigRejection::new(reason, None)).into())
}

/// 私有 IPC 录入材料，不提供 Debug，也不进入公开响应。
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub(super) struct AuthKey(pub(super) String);

pub(super) fn error(error: lattice_llm::auth::AuthError) -> super::LocalError {
    let kind = match &error {
        lattice_llm::auth::AuthError::Io {
            kind: std::io::ErrorKind::PermissionDenied,
            ..
        } => super::LocalErrorKind::Permission,
        lattice_llm::auth::AuthError::Io { .. } => super::LocalErrorKind::Internal,
        lattice_llm::auth::AuthError::Internal(_) => super::LocalErrorKind::Internal,
        _ => super::LocalErrorKind::BadRequest,
    };
    super::LocalError::new(kind, &error.to_string())
}

#[cfg(test)]
mod tests;
