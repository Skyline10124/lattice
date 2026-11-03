// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex, MutexGuard};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, watch};

use super::{Credentials, ProviderAuthStatus, Secret, UpdateError, format};
use crate::LlmError;

/// 只在专用登录交互中借用，不进入 Debug、状态或通用序列化。
pub struct SensitiveLoginUrl(Secret);

impl SensitiveLoginUrl {
    pub fn new(url: String) -> Self {
        Self(Secret(url))
    }
    pub fn expose(&self) -> &str {
        &self.0.0
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum LoginError {
    #[error("ChatGPT login cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(#[from] LlmError),
}

/// 句柄释放即取消未提交的登录；任务由同一个 ChatgptAuth 监督并排空。
pub struct ChatgptLogin {
    pub(super) authorization: SensitiveLoginUrl,
    pub(super) input: mpsc::Sender<SensitiveLoginUrl>,
    pub(super) completion: watch::Receiver<Option<Result<ProviderAuthStatus, LoginError>>>,
    pub(super) control: Arc<LoginControl>,
}

impl ChatgptLogin {
    pub fn authorization_url(&self) -> &SensitiveLoginUrl {
        &self.authorization
    }

    pub async fn submit_callback(&self, url: SensitiveLoginUrl) -> Result<(), LoginError> {
        self.input
            .send(url)
            .await
            .map_err(|_| failure("login is no longer waiting").into())
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// 等待结果可以取消后再等待；持有句柄期间登录仍然有效。
    pub async fn wait(&self) -> Result<ProviderAuthStatus, LoginError> {
        let mut receiver = self.completion.clone();
        loop {
            if let Some(result) = receiver.borrow_and_update().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| failure("login worker failed"))?;
        }
    }
}

impl Drop for ChatgptLogin {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(PartialEq, Eq)]
pub(super) enum Phase {
    Pending,
    Cancelled,
    Published,
}

pub(super) struct LoginControl {
    phase: Mutex<Phase>,
    cancellation: watch::Sender<bool>,
}

impl LoginControl {
    pub(super) fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Pending),
            cancellation: watch::channel(false).0,
        }
    }

    pub(super) fn cancel(&self) {
        let mut phase = self.phase.lock().expect("login publication lock");
        if *phase == Phase::Pending {
            *phase = Phase::Cancelled;
            self.cancellation.send_replace(true);
        }
    }

    pub(super) async fn cancelled(&self) {
        let mut receiver = self.cancellation.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    pub(super) fn is_cancelled(&self) -> bool {
        *self.cancellation.borrow()
    }

    pub(super) fn publication(&self) -> Result<MutexGuard<'_, Phase>, UpdateError> {
        let phase = self.phase.lock().expect("login publication lock");
        if *phase != Phase::Pending {
            return Err(UpdateError::Conflict);
        }
        Ok(phase)
    }
}

pub(super) struct Pending {
    pub host: Secret,
    pub client: Option<Secret>,
    pub subject: Option<Secret>,
    hint: Option<Secret>,
    pub state: Secret,
    pub nonce: Secret,
    pub verifier: Secret,
    pub redirect: String,
}

impl Pending {
    pub(super) fn new(credentials: &Credentials, redirect: String) -> Result<Self, LlmError> {
        let Some(format::Credential::Chatgpt {
            host_id,
            client_id,
            account,
            tokens,
        }) = credentials.0.providers.get("chatgpt")
        else {
            return Err(failure("host identity was not saved"));
        };
        Ok(Self {
            host: Secret(host_id.0.clone()),
            client: client_id.as_ref().map(|value| Secret(value.0.clone())),
            subject: account
                .as_ref()
                .map(|value| Secret(value.subject.0.clone())),
            hint: tokens
                .as_ref()
                .map(|value| Secret(value.id_token.0.clone())),
            state: random_secret()?,
            nonce: random_secret()?,
            verifier: random_secret()?,
            redirect,
        })
    }

    pub(super) fn authorization(&self) -> SensitiveLoginUrl {
        let mut url = url::Url::parse("https://auth.openai.com/api/accounts/authorize")
            .expect("fixed authorization URL");
        let challenge = challenge(&self.verifier.0);
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs([
                ("response_type", "code"),
                ("redirect_uri", &self.redirect),
                ("resource", "https://api.openai.com/v1"),
                (
                    "scope",
                    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                ),
                ("ext_agent_host_id", &self.host.0),
                ("state", &self.state.0),
                ("nonce", &self.nonce.0),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
            ]);
            if let Some(client) = &self.client {
                query.append_pair("client_id", &client.0);
                if let Some(hint) = &self.hint {
                    query.append_pair("id_token_hint", &hint.0);
                }
            } else {
                query.append_pair("client_id", "dynamic_agent_client");
                query.append_pair("agent_name_hint", "LATTICE");
            }
        }
        SensitiveLoginUrl::new(url.into())
    }

    pub(super) fn matches(&self, credentials: &Credentials) -> bool {
        matches!(credentials.0.providers.get("chatgpt"),
            Some(format::Credential::Chatgpt { host_id, client_id, account, .. })
                if host_id == &self.host && client_id.as_ref() == self.client.as_ref()
                    && account.as_ref().map(|account| &account.subject) == self.subject.as_ref())
    }

    pub(super) fn redact(
        &self,
        error: LlmError,
        credentials: &Credentials,
        extra: &[&str],
    ) -> LlmError {
        let mut values = credentials.0.values();
        values.extend([
            self.host.0.as_str(),
            &self.state.0,
            &self.nonce.0,
            &self.verifier.0,
        ]);
        for value in [&self.client, &self.subject, &self.hint]
            .into_iter()
            .flatten()
        {
            values.push(&value.0);
        }
        values.extend_from_slice(extra);
        values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        crate::client::redact_error_with(error, |message| {
            let mut message = message.to_owned();
            for value in values {
                if !value.is_empty() {
                    message = message.replace(value, "[REDACTED]");
                }
            }
            crate::transport::bound_diagnostic(&message)
        })
    }
}

pub(super) fn host_id() -> Result<Secret, LlmError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure("system randomness unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(Secret(uuid::Uuid::from_bytes(bytes).urn().to_string()))
}

fn random_secret() -> Result<Secret, LlmError> {
    let mut bytes = zeroize::Zeroizing::new([0; 32]);
    getrandom::fill(&mut *bytes).map_err(|_| failure("system randomness unavailable"))?;
    Ok(Secret(URL_SAFE_NO_PAD.encode(bytes.as_slice())))
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub(super) fn failure(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT login: {reason}"),
    }
}

#[cfg(test)]
mod tests;
