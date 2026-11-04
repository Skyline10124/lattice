// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, Mutex, OnceLock};

use chrono::{TimeDelta, Utc};
use tokio::sync::{Notify, oneshot, watch};

use super::oauth::{
    Clock, DIRECT_SCOPE, HttpTokenEndpoint, RefreshFailure, RefreshGrant, TokenEndpoint,
};
use super::{AuthError, AuthStore, Credentials, UpdateError, format, valid_access_token};
use crate::LlmError;

/// 网关共享的 SIWC 生命周期句柄；所有秘密与刷新细节留在本库内。
#[derive(Clone)]
pub struct ChatgptAuth(Arc<Manager>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationStatus {
    Confirmed,
    Unconfirmed,
    NotNeeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoveChatgptResult {
    pub removed: bool,
    pub revocation: RevocationStatus,
}

/// 只在请求网关内部使用；不提供 Debug 或序列化。
pub(crate) struct AccessToken {
    pub(crate) credentials: Arc<Credentials>,
}

impl AccessToken {
    pub(crate) fn authorization(&self) -> Result<reqwest::header::HeaderValue, LlmError> {
        let (_, tokens) = material(&self.credentials)?;
        let value = zeroize::Zeroizing::new(format!("Bearer {}", tokens.access_token.0));
        let mut header = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| credential_error("invalid access token in auth.json"))?;
        header.set_sensitive(true);
        Ok(header)
    }
}

struct Manager {
    store: AuthStore,
    endpoint: Arc<dyn TokenEndpoint>,
    clock: Clock,
    state: Mutex<State>,
    operation: tokio::sync::Mutex<()>,
    changed: Notify,
}

#[derive(Default)]
struct State {
    generation: u64,
    next_flight: u64,
    flight: Option<Arc<Flight>>,
    cleaning: usize,
    active: usize,
    stopped: bool,
    worker_failed: bool,
    blocked_generation: Option<u64>,
}

struct Flight {
    id: u64,
    generation: u64,
    completion: watch::Receiver<Option<Result<u64, LlmError>>>,
    credentials: OnceLock<Arc<Credentials>>,
}

impl Flight {
    async fn wait(&self) -> Result<u64, LlmError> {
        let mut completion = self.completion.clone();
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            completion
                .changed()
                .await
                .map_err(|_| credential_error("credential worker failed"))?;
        }
    }
}

impl ChatgptAuth {
    pub(crate) fn new(store: AuthStore, roots: Vec<reqwest::Certificate>) -> Self {
        let clock: Clock = Arc::new(Utc::now);
        let endpoint = Arc::new(HttpTokenEndpoint {
            roots,
            clock: clock.clone(),
        });
        Self::with_dependencies(store, endpoint, clock)
    }

    fn with_dependencies(store: AuthStore, endpoint: Arc<dyn TokenEndpoint>, clock: Clock) -> Self {
        Self(Arc::new(Manager {
            store,
            endpoint,
            clock,
            state: Mutex::default(),
            operation: tokio::sync::Mutex::new(()),
            changed: Notify::new(),
        }))
    }

    /// 每次使用前取得可用 token；等待者取消不撤销已经接纳的轮换与写回。
    pub(crate) async fn access_token(&self) -> Result<AccessToken, LlmError> {
        let flight = {
            let mut state = self.0.state.lock().expect("SIWC state lock");
            if state.stopped || state.cleaning != 0 {
                return Err(credential_error(
                    "credential lifecycle is stopping or signing out",
                ));
            }
            if let Some(flight) = &state.flight {
                flight.clone()
            } else {
                let (completion, receiver) = watch::channel(None);
                state.next_flight += 1;
                let flight = Arc::new(Flight {
                    id: state.next_flight,
                    generation: state.generation,
                    completion: receiver,
                    credentials: OnceLock::new(),
                });
                state.flight = Some(flight.clone());
                state.active += 1;
                let worker_flight = flight.clone();
                let manager = self.0.clone();
                tokio::spawn(async move {
                    let mut worker = Worker::new(manager.clone(), Some(worker_flight.id));
                    let result = manager.read_or_refresh(worker_flight.generation).await;
                    let result = result.map(|(generation, credentials)| {
                        let _ = worker_flight.credentials.set(credentials);
                        generation
                    });
                    completion.send_replace(Some(result));
                    worker.finished = true;
                });
                flight
            }
        };
        let generation = flight.wait().await?;
        let state = self.0.state.lock().expect("SIWC state lock");
        if state.generation != generation || state.cleaning != 0 || state.stopped {
            return Err(credential_error("credentials changed before use"));
        }
        Ok(AccessToken {
            credentials: flight
                .credentials
                .get()
                .ok_or_else(|| credential_error("credential worker returned no material"))?
                .clone(),
        })
    }

    /// 只尝试一次远端撤销；成功清除本地 token 后保留注册、账号和 host。
    pub async fn logout(&self) -> Result<RevocationStatus, AuthError> {
        Ok(self.clean(false).await?.revocation)
    }

    /// 移除注册与账号并删除账号模型缓存，保留已经存在的 host。
    pub async fn remove(&self) -> Result<RemoveChatgptResult, AuthError> {
        self.clean(true).await
    }

    async fn clean(&self, remove: bool) -> Result<RemoveChatgptResult, AuthError> {
        let (sender, receiver) = oneshot::channel();
        let flight = {
            let mut state = self.0.state.lock().expect("SIWC state lock");
            if state.stopped {
                return Err(AuthError::Internal("SIWC is stopping"));
            }
            state.cleaning += 1;
            state.active += 1;
            state.flight.clone()
        };
        let manager = self.0.clone();
        tokio::spawn(async move {
            let mut worker = Worker::new(manager.clone(), None);
            // 先接收可能已轮换的最新材料，再进入清理事务，避免撤销旧 refresh token。
            if let Some(flight) = flight {
                let _ = flight.wait().await;
            }
            let result = manager.clean(remove).await;
            let _ = sender.send(result);
            worker.finished = true;
        });
        receiver
            .await
            .map_err(|_| AuthError::Internal("SIWC cleanup worker failed"))?
    }

    /// 关闭新操作并排空已接纳的网络操作及必要文件写入。
    pub async fn shutdown(&self) -> Result<(), AuthError> {
        self.0.state.lock().expect("SIWC state lock").stopped = true;
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.0.state.lock().expect("SIWC state lock");
                if state.active == 0 {
                    return if state.worker_failed {
                        Err(AuthError::Internal("SIWC worker failed"))
                    } else {
                        Ok(())
                    };
                }
            }
            changed.await;
        }
    }
}

impl Manager {
    async fn load(&self) -> Result<Credentials, AuthError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.load())
            .await
            .map_err(|_| AuthError::Internal("credential read worker failed"))?
    }

    async fn update(
        &self,
        expected: Arc<Credentials>,
        update: impl FnOnce(&mut format::Credential) -> bool + Send + 'static,
    ) -> Result<Arc<Credentials>, UpdateError> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.update_chatgpt(&expected, update).map(Arc::new))
            .await
            .map_err(|_| {
                UpdateError::Storage(AuthError::Internal("credential update worker failed"))
            })?
    }

    async fn read_or_refresh(&self, generation: u64) -> Result<(u64, Arc<Credentials>), LlmError> {
        let _operation = self.operation.lock().await;
        {
            let state = self.state.lock().expect("SIWC state lock");
            if state.generation != generation || state.blocked_generation == Some(generation) {
                return Err(credential_error(
                    "credential generation is no longer usable; sign in again",
                ));
            }
        }
        let credentials = Arc::new(self.load().await.map_err(storage_error)?);
        let (client_id, tokens) = material(&credentials)?;
        let expiry = chrono::DateTime::parse_from_rfc3339(&tokens.expires_at)
            .map_err(|_| storage_error(AuthError::InvalidFile("invalid expiry")))?;
        if expiry.signed_duration_since((self.clock)()) >= TimeDelta::seconds(300) {
            return Ok((generation, credentials));
        }
        let grant = RefreshGrant {
            client_id,
            refresh_token: &tokens.refresh_token.0,
        };
        let result = match self.endpoint.refresh(grant).await {
            Ok(reply) => {
                self.update(credentials.clone(), move |entry| {
                    let format::Credential::Chatgpt {
                        tokens: Some(tokens),
                        ..
                    } = entry
                    else {
                        unreachable!("compared token record remains present")
                    };
                    tokens.access_token = reply.access_token;
                    tokens.refresh_token = reply.refresh_token;
                    tokens.expires_at = reply.expires_at;
                    tokens.scopes = reply.scopes;
                    true
                })
                .await
            }
            Err(RefreshFailure::InvalidToken(code)) => {
                if let Err(error) = self.update(credentials.clone(), clear_tokens).await {
                    return Err(self.failed_update(generation, &credentials, error).await);
                }
                self.advance_generation();
                return Err(credential_error(&format!(
                    "token refresh failed ({code}); run lattice auth login chatgpt"
                )));
            }
            Err(RefreshFailure::Other(error)) => return Err(refresh_error(error, &credentials)),
        };
        match result {
            Ok(credentials) => Ok((self.advance_generation(), credentials)),
            Err(error) => Err(self.failed_update(generation, &credentials, error).await),
        }
    }

    async fn failed_update(
        &self,
        generation: u64,
        expected: &Credentials,
        error: UpdateError,
    ) -> LlmError {
        let error = match error {
            UpdateError::Conflict => {
                return credential_error("credentials changed during refresh; retry the request");
            }
            UpdateError::Storage(error) => error,
        };
        // rename 后的错误不代表旧记录仍在；重读只核对可见状态，不宣称持久化成功。
        self.state
            .lock()
            .expect("SIWC state lock")
            .blocked_generation = Some(generation);
        let observation = match self.load().await {
            Ok(current)
                if current.0.providers.get("chatgpt") == expected.0.providers.get("chatgpt") =>
            {
                "previous credential record is still visible"
            }
            Ok(_) => "credential record changed; durability was not confirmed",
            Err(_) => "credential record could not be reread",
        };
        LlmError::Config {
            message: crate::transport::bound_diagnostic(&format!(
                "ChatGPT credential storage failed: {error}; {observation}; sign in again"
            )),
        }
    }

    fn advance_generation(&self) -> u64 {
        let mut state = self.state.lock().expect("SIWC state lock");
        state.generation += 1;
        state.blocked_generation = None;
        state.generation
    }

    async fn clean(&self, remove: bool) -> Result<RemoveChatgptResult, AuthError> {
        let _operation = self.operation.lock().await;
        let generation = self.advance_generation();
        self.state
            .lock()
            .expect("SIWC state lock")
            .blocked_generation = Some(generation);
        let credentials = Arc::new(self.load().await?);
        let removed = matches!(credentials.0.providers.get("chatgpt"), Some(format::Credential::Chatgpt {
            client_id, account, tokens, ..
        }) if client_id.is_some() || account.is_some() || tokens.is_some());
        let revocation = match credentials.0.providers.get("chatgpt") {
            Some(format::Credential::Chatgpt {
                client_id: Some(client),
                tokens: Some(tokens),
                ..
            }) => {
                match self
                    .endpoint
                    .revoke(RefreshGrant {
                        client_id: &client.0,
                        refresh_token: &tokens.refresh_token.0,
                    })
                    .await
                {
                    Ok(()) => RevocationStatus::Confirmed,
                    Err(_) => RevocationStatus::Unconfirmed,
                }
            }
            _ => RevocationStatus::NotNeeded,
        };
        let credentials = self
            .update(credentials, clear_tokens)
            .await
            .map_err(|error| self.cleanup_update_error(error))?;
        if remove {
            let cache = self.store.directory.join("cache/chatgpt-models.json");
            tokio::task::spawn_blocking(move || match std::fs::remove_file(cache) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(AuthError::io("remove ChatGPT model cache", error)),
            })
            .await
            .map_err(|_| AuthError::Internal("model cache removal worker failed"))??;
            self.update(credentials, |entry| {
                let format::Credential::Chatgpt {
                    client_id, account, ..
                } = entry
                else {
                    unreachable!("ChatGPT entry has the expected type")
                };
                let changed = client_id.is_some() || account.is_some();
                *client_id = None;
                *account = None;
                changed
            })
            .await
            .map_err(|error| self.cleanup_update_error(error))?;
        }
        self.state
            .lock()
            .expect("SIWC state lock")
            .blocked_generation = None;
        Ok(RemoveChatgptResult {
            removed,
            revocation,
        })
    }

    fn cleanup_update_error(&self, error: UpdateError) -> AuthError {
        match error {
            UpdateError::Conflict => {
                self.state
                    .lock()
                    .expect("SIWC state lock")
                    .blocked_generation = None;
                AuthError::Internal("ChatGPT credentials changed during operation")
            }
            UpdateError::Storage(error) => error,
        }
    }
}

struct Worker {
    manager: Arc<Manager>,
    flight: Option<u64>,
    finished: bool,
}

impl Worker {
    fn new(manager: Arc<Manager>, flight: Option<u64>) -> Self {
        Self {
            manager,
            flight,
            finished: false,
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let mut state = self.manager.state.lock().expect("SIWC state lock");
        if let Some(id) = self.flight {
            if state.flight.as_ref().is_some_and(|flight| flight.id == id) {
                state.flight = None;
            }
        } else {
            state.cleaning -= 1;
        }
        state.active -= 1;
        if !self.finished {
            state.worker_failed = true;
            state.blocked_generation = Some(state.generation);
        }
        self.manager.changed.notify_waiters();
    }
}

fn material(credentials: &Credentials) -> Result<(&str, &format::Tokens), LlmError> {
    match credentials.0.providers.get("chatgpt") {
        Some(format::Credential::Chatgpt {
            client_id: Some(client),
            tokens: Some(tokens),
            ..
        }) if client.0 != "dynamic_agent_client"
            && tokens.scopes.iter().any(|scope| scope == DIRECT_SCOPE)
            && valid_access_token(&tokens.access_token.0) =>
        {
            Ok((&client.0, tokens))
        }
        _ => Err(credential_error(
            "not signed in or missing direct permission; run lattice auth login chatgpt",
        )),
    }
}

fn clear_tokens(entry: &mut format::Credential) -> bool {
    let format::Credential::Chatgpt { tokens, .. } = entry else {
        unreachable!("ChatGPT entry has the expected type")
    };
    tokens.take().is_some()
}

fn credential_error(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT: {reason}"),
    }
}

fn storage_error(error: AuthError) -> LlmError {
    LlmError::Config {
        message: format!("ChatGPT credential storage failed: {error}"),
    }
}

fn refresh_error(error: LlmError, credentials: &Credentials) -> LlmError {
    let mut error = crate::client::redact_error(error, credentials);
    match &mut error {
        LlmError::Credential { message }
        | LlmError::Config { message }
        | LlmError::Dns { message }
        | LlmError::Transport { message, .. }
        | LlmError::Timeout { message }
        | LlmError::ProviderHttp { message, .. }
        | LlmError::StreamRead { message }
        | LlmError::ProtocolLimit { message }
        | LlmError::Other(message) => {
            *message = crate::transport::bound_diagnostic(message);
        }
        LlmError::Projection(_) => {}
    }
    error
}

#[cfg(test)]
mod tests;
