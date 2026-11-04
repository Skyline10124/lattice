// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::net::{Ipv4Addr, SocketAddr};

use super::*;
use crate::auth::{
    AuthState, ProviderAuthStatus, Secret,
    callback::{self, Listener},
    id_token,
    login::{ChatgptLogin, LoginError, Pending, Phase, SensitiveLoginUrl, failure, host_id},
    oauth::{CodeGrant, ExchangeFailure},
};
use tokio::sync::mpsc;

struct CancelOnDrop(Option<Arc<LoginControl>>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(control) = &self.0 {
            control.cancel();
        }
    }
}

impl ChatgptAuth {
    /// 启动固定 loopback 登录；句柄只携带专用交互所需的敏感授权信息。
    pub async fn start_login(&self) -> Result<ChatgptLogin, LoginError> {
        self.start_login_at(SocketAddr::from((Ipv4Addr::LOCALHOST, 1455)))
            .await
    }

    async fn start_login_at(&self, address: SocketAddr) -> Result<ChatgptLogin, LoginError> {
        let control = Arc::new(LoginControl::new());
        {
            let mut state = self.0.state.lock().expect("SIWC state lock");
            if state.stopped || state.cleaning != 0 || state.login.is_some() {
                return Err(
                    failure("lifecycle is stopping, signing out or already signing in").into(),
                );
            }
            state.login = Some(control.clone());
            state.active += 1;
        }
        let mut startup = CancelOnDrop(Some(control.clone()));
        let (ready, authorization) = oneshot::channel();
        let (input, receiver) = mpsc::channel(1);
        let (completion, result) = watch::channel(None);
        let manager = self.0.clone();
        let worker_control = control.clone();
        tokio::spawn(async move {
            let mut worker = Worker::new(manager.clone(), Work::Login(worker_control.clone()));
            let outcome = manager
                .run_login(&worker_control, address, receiver, ready)
                .await;
            worker.finished = true;
            drop(worker);
            completion.send_replace(Some(outcome));
        });
        let authorization = match authorization.await {
            Ok(url) => url,
            Err(_) => {
                let mut result = result.clone();
                loop {
                    if let Some(outcome) = result.borrow_and_update().clone() {
                        return Err(outcome
                            .err()
                            .unwrap_or_else(|| failure("login startup failed").into()));
                    }
                    result
                        .changed()
                        .await
                        .map_err(|_| failure("login worker failed"))?;
                }
            }
        };
        // 取消责任从准备 future 交给返回的句柄。
        let login = ChatgptLogin {
            authorization,
            input,
            completion: result,
            control,
        };
        startup.0.take();
        Ok(login)
    }
}

impl Manager {
    async fn run_login(
        self: &Arc<Self>,
        control: &Arc<LoginControl>,
        address: SocketAddr,
        mut input: mpsc::Receiver<SensitiveLoginUrl>,
        ready: oneshot::Sender<SensitiveLoginUrl>,
    ) -> Result<ProviderAuthStatus, LoginError> {
        let (credentials, pending, listener) = self.prepare_login(control, address).await?;
        if ready.send(pending.authorization()).is_err() {
            return Err(LoginError::Cancelled);
        }
        let callback = tokio::select! {
            biased;
            _ = control.cancelled() => return Err(LoginError::Cancelled),
            callback = listener.receive() => callback?,
            callback = input.recv() => callback.ok_or(LoginError::Cancelled)?,
        };
        drop(listener);
        drop(input);
        let code = callback::parse(
            &callback,
            &pending.redirect,
            &pending.state.0,
            pending.client.as_ref().map(|client| client.0.as_str()),
        )
        .map_err(|error| pending.redact(error, &credentials, &[callback.expose()]))?;
        let exchange = tokio::select! {
            biased;
            _ = control.cancelled() => return Err(LoginError::Cancelled),
            result = self.endpoint.exchange(CodeGrant {
                client_id: &code.client_id.0,
                code: &code.code.0,
                verifier: &pending.verifier.0,
                redirect_uri: &pending.redirect,
            }) => result,
        };
        let tokens = match exchange {
            Ok(tokens) => tokens,
            Err(ExchangeFailure::InvalidGrant) => {
                if pending.client.is_none() {
                    self.save_login(
                        control,
                        &pending,
                        format::Credential::Chatgpt {
                            host_id: Secret(pending.host.0.clone()),
                            client_id: Some(code.client_id),
                            account: None,
                            tokens: None,
                        },
                    )
                    .await?;
                }
                return Err(
                    failure("code exchange returned invalid_grant; start a fresh login").into(),
                );
            }
            Err(ExchangeFailure::Other(error)) => {
                return Err(pending
                    .redact(
                        error,
                        &credentials,
                        &[callback.expose(), &code.code.0, &code.client_id.0],
                    )
                    .into());
            }
        };
        let identity = tokio::select! {
            biased;
            _ = control.cancelled() => return Err(LoginError::Cancelled),
            identity = id_token::verify(&tokens.id_token.0, &code.client_id.0, &pending.nonce.0, &*self.endpoint) => identity,
        }.map_err(|error| pending.redact(error, &credentials, &[
            callback.expose(), &code.code.0, &code.client_id.0, &tokens.token.access_token.0,
            &tokens.token.refresh_token.0, &tokens.id_token.0,
        ]))?;
        if pending
            .subject
            .as_ref()
            .is_some_and(|subject| subject != &identity.subject)
            || !tokens
                .token
                .scopes
                .iter()
                .any(|scope| scope == DIRECT_SCOPE)
        {
            return Err(failure("account mismatch or missing direct permission").into());
        }
        let status = ProviderAuthStatus {
            provider: "chatgpt".into(),
            status: AuthState::Ready,
            email: Some(identity.email.clone()),
            expires_at: Some(tokens.token.expires_at.clone()),
        };
        self.save_login(
            control,
            &pending,
            format::Credential::Chatgpt {
                host_id: Secret(pending.host.0.clone()),
                client_id: Some(code.client_id),
                account: Some(identity),
                tokens: Some(format::Tokens {
                    access_token: tokens.token.access_token,
                    refresh_token: tokens.token.refresh_token,
                    id_token: tokens.id_token,
                    expires_at: tokens.token.expires_at,
                    scopes: tokens.token.scopes,
                }),
            },
        )
        .await?;
        Ok(status)
    }

    async fn prepare_login(
        self: &Arc<Self>,
        control: &Arc<LoginControl>,
        address: SocketAddr,
    ) -> Result<(Arc<Credentials>, Pending, Listener), LoginError> {
        let _operation = self.operation.lock().await;
        self.check_login(control)?;
        let mut credentials = Arc::new(self.load().await.map_err(storage_error)?);
        if !credentials.0.providers.contains_key("chatgpt") {
            let replacement = format::Credential::Chatgpt {
                host_id: host_id()?,
                client_id: None,
                account: None,
                tokens: None,
            };
            credentials = self
                .publish_login(control, credentials, replacement, false)
                .await?;
        }
        self.check_login(control)?;
        let listener = Listener::bind(address).await?;
        let pending = Pending::new(&credentials, listener.redirect_uri().into())?;
        Ok((credentials, pending, listener))
    }

    fn check_login(&self, control: &Arc<LoginControl>) -> Result<u64, LoginError> {
        let state = self.state.lock().expect("SIWC state lock");
        if control.is_cancelled()
            || state.stopped
            || state.cleaning != 0
            || !state
                .login
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, control))
        {
            return Err(LoginError::Cancelled);
        }
        Ok(state.generation)
    }

    async fn save_login(
        self: &Arc<Self>,
        control: &Arc<LoginControl>,
        pending: &Pending,
        replacement: format::Credential,
    ) -> Result<(), LoginError> {
        let _operation = self.operation.lock().await;
        self.check_login(control)?;
        let credentials = Arc::new(self.load().await.map_err(storage_error)?);
        if !pending.matches(&credentials) {
            return Err(failure("registration changed during login").into());
        }
        self.publish_login(control, credentials, replacement, true)
            .await
            .map(|_| ())
    }

    async fn publish_login(
        self: &Arc<Self>,
        control: &Arc<LoginControl>,
        expected: Arc<Credentials>,
        replacement: format::Credential,
        final_commit: bool,
    ) -> Result<Arc<Credentials>, LoginError> {
        let generation = self.check_login(control)?;
        let manager = self.clone();
        let control_write = control.clone();
        let expected_write = expected.clone();
        let result = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(gate) = manager.before_login_write.lock().unwrap().take() {
                gate();
            }
            manager
                .store
                .replace_chatgpt(
                    &expected_write,
                    replacement,
                    || {
                        let phase = control_write.publication()?;
                        if manager.check_login(&control_write).ok() != Some(generation) {
                            return Err(UpdateError::Conflict);
                        }
                        Ok(phase)
                    },
                    |phase| {
                        if final_commit {
                            **phase = Phase::Published;
                        }
                    },
                )
                .map(Arc::new)
        })
        .await
        .map_err(|_| UpdateError::Storage(AuthError::Internal("login write worker failed")))
        .and_then(|result| result);
        match result {
            Ok(credentials) => {
                self.advance_generation();
                Ok(credentials)
            }
            Err(UpdateError::Conflict) if self.check_login(control).is_err() => {
                Err(LoginError::Cancelled)
            }
            Err(UpdateError::Conflict) => {
                Err(failure("credentials changed during login; start a fresh login").into())
            }
            Err(UpdateError::Storage(error)) => Err(self
                .failed_update(generation, &expected, UpdateError::Storage(error))
                .await
                .into()),
        }
    }
}

#[cfg(test)]
mod tests;
