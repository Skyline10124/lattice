// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub(super) mod login;

pub(super) use lattice_ipc::AuthKey;

pub(super) fn require_chatgpt(provider: &str) -> super::Result<()> {
    if provider == "chatgpt" {
        return Ok(());
    }
    Err(super::LocalError::new(
        super::LocalErrorKind::BadRequest,
        "login and logout require the built-in chatgpt provider",
    ))
}

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
