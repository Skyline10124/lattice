// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 凭据录入材料、登录流消息与凭据状态展示类型。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{
    models::Availability,
    types::{LocalError, LocalErrorKind, Result},
};

/// 私有 IPC 录入材料，不提供 Debug，也不进入公开响应。
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(transparent)]
pub struct AuthKey(String);

impl AuthKey {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RevocationStatus {
    Confirmed,
    Unconfirmed,
    NotNeeded,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderAuthStatus {
    pub provider: String,
    pub availability: Availability,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthStatus {
    pub providers_path: PathBuf,
    pub providers: Vec<ProviderAuthStatus>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthLoginOutcome {
    SignedIn {
        status: ProviderAuthStatus,
        catalog: AuthCatalogOutcome,
    },
    Cancelled {},
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthCatalogOutcome {
    Refreshed { model_count: usize },
    Failed { warning: String },
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "response",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum LoginResponse {
    AuthLoginAuthorization { authorization_url: AuthKey },
    AuthLoginResult(AuthLoginOutcome),
    Error(LocalError),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoginAction {
    Callback { url: AuthKey },
    Cancel {},
}

/// 登录与登出协议只支持内置 `chatgpt` provider。
pub fn require_chatgpt(provider: &str) -> Result<()> {
    if provider == "chatgpt" {
        return Ok(());
    }
    Err(LocalError::new(
        LocalErrorKind::BadRequest,
        "login and logout require the built-in chatgpt provider",
    ))
}
