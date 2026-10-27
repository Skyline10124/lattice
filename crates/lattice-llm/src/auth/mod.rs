// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台凭据库：按当前注册表校验操作，严格读取并原子替换 auth.json。

use std::{collections::BTreeMap, fmt, path::PathBuf, sync::Mutex};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::provider::{Auth, config::ProviderRegistry};

mod file;
mod format;

const MAX_KEY_BYTES: usize = 16_384;
const MAX_FILE_BYTES: usize = 1_048_576;
// 锁覆盖读取、修改和发布；多个库句柄也不能覆盖彼此的更新。
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("auth.json: {0}")]
    InvalidFile(&'static str),
    #[error("API key must be 1–16384 bytes and contain no control characters")]
    InvalidKey,
    #[error("{0}")]
    Internal(&'static str),
    #[error("auth.json: {operation} failed ({kind:?})")]
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    #[error("{path}:{line}: {reason}", path = .path.display())]
    Providers {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error(
        "provider {provider:?} is not registered; available providers for auth set: {available}"
    )]
    UnknownProvider { provider: String, available: String },
    #[error("provider does not accept API keys; auth must be bearer, x-api-key or x-goog-api-key")]
    UnsupportedAuth,
    #[error("ChatGPT credential removal is not supported")]
    ChatgptRemoval,
}

impl AuthError {
    fn io(operation: &'static str, error: impl Into<std::io::Error>) -> Self {
        Self::Io {
            operation,
            kind: error.into().kind(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Ready,
    Missing,
    Invalid,
}

impl fmt::Display for AuthState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderAuthStatus {
    pub provider: String,
    pub status: AuthState,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthStatus {
    pub providers: Vec<ProviderAuthStatus>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

/// 凭据值仅用于内部序列化；Debug 不展示内容，释放时清零。
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

fn validate_key(key: &str) -> Result<(), AuthError> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(AuthError::InvalidKey);
    }
    Ok(())
}

/// 只持配置目录；每次操作按需读取当前文件，不缓存秘密。
#[derive(Debug, Clone)]
pub struct AuthStore {
    directory: PathBuf,
}

impl AuthStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn registry(&self) -> Result<ProviderRegistry, AuthError> {
        ProviderRegistry::load(self.directory.join("providers.toml")).map_err(|error| {
            AuthError::Providers {
                path: error.path,
                line: error.line,
                reason: error.reason,
            }
        })
    }

    pub fn set(&self, provider: &str, key: &str) -> Result<(), AuthError> {
        let _writer = WRITER
            .lock()
            .map_err(|_| AuthError::Internal("credential writer lock is unavailable"))?;
        let registry = self.registry()?;
        let provider = registry
            .get(provider)
            .ok_or_else(|| AuthError::UnknownProvider {
                provider: provider.into(),
                available: registry
                    .iter()
                    .filter(|provider| {
                        matches!(
                            provider.auth,
                            Auth::Bearer | Auth::XApiKey | Auth::XGoogApiKey
                        )
                    })
                    .map(|provider| provider.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
        if !matches!(
            provider.auth,
            Auth::Bearer | Auth::XApiKey | Auth::XGoogApiKey
        ) {
            return Err(AuthError::UnsupportedAuth);
        }
        validate_key(key)?;
        let directory = file::Directory::open(&self.directory, true)?
            .ok_or(AuthError::Internal("configuration directory disappeared"))?;
        let mut document = directory.read()?;
        document.providers.insert(
            provider.id.clone(),
            format::Credential::ApiKey {
                key: Secret(key.to_owned()),
            },
        );
        directory.write(&document)
    }

    pub fn remove(&self, provider: &str) -> Result<bool, AuthError> {
        let _writer = WRITER
            .lock()
            .map_err(|_| AuthError::Internal("credential writer lock is unavailable"))?;
        // 删除不要求仍在注册表中，便于清理已从 providers.toml 移除的 API key。
        let directory = file::Directory::open(&self.directory, false)?;
        let mut document = match directory.as_ref() {
            Some(directory) => directory.read()?,
            None => format::Document::default(),
        };
        if provider == "chatgpt"
            || matches!(
                document.providers.get(provider),
                Some(format::Credential::Chatgpt { .. })
            )
        {
            return Err(AuthError::ChatgptRemoval);
        }
        let removed = document.providers.remove(provider).is_some();
        if removed {
            directory
                .expect("existing credential has a directory")
                .write(&document)?;
        }
        Ok(removed)
    }

    pub fn status(&self) -> Result<AuthStatus, AuthError> {
        let registry = self.registry()?;
        let document = file::Directory::open(&self.directory, false).and_then(|directory| {
            directory.map_or_else(
                || Ok(format::Document::default()),
                |directory| directory.read(),
            )
        });
        let error = document.as_ref().err().map(ToString::to_string);
        let providers = registry
            .iter()
            .map(|provider| {
                let mut row = ProviderAuthStatus {
                    provider: provider.id.clone(),
                    status: AuthState::Missing,
                    email: None,
                    expires_at: None,
                };
                if provider.auth == Auth::None {
                    row.status = AuthState::Ready;
                } else if let Ok(document) = &document {
                    match document.providers.get(&provider.id) {
                        Some(format::Credential::ApiKey { .. })
                            if provider.auth != Auth::OauthChatgpt =>
                        {
                            row.status = AuthState::Ready
                        }
                        Some(format::Credential::Chatgpt {
                            account, tokens, ..
                        }) if provider.auth == Auth::OauthChatgpt => {
                            row.email = account.as_ref().map(|account| account.email.clone());
                            row.expires_at =
                                tokens.as_ref().map(|tokens| tokens.expires_at.clone());
                            if tokens.is_some() {
                                row.status = AuthState::Ready;
                            }
                        }
                        _ => {}
                    }
                } else {
                    row.status = AuthState::Invalid;
                }
                row
            })
            .collect();
        Ok(AuthStatus { providers, error })
    }
}

#[cfg(test)]
mod tests;
