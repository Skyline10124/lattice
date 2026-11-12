// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use futures::future::BoxFuture;
use lattice_llm::auth::{ChatgptLogin, LoginError, SensitiveLoginUrl};
use tokio::{net::UnixStream, sync::mpsc, task::JoinHandle};

use super::AuthKey;
use crate::local::{
    ipc,
    types::{LocalError, LocalErrorKind, Result},
};

pub(in crate::local) use lattice_ipc::{
    AuthCatalogOutcome, AuthLoginOutcome, LoginAction, LoginResponse,
};

fn protocol() -> LocalError {
    LocalError::new(LocalErrorKind::Protocol, "unexpected login message")
}

pub(in crate::local) trait LoginAttempt: Send + Sync {
    fn authorization_url(&self) -> &SensitiveLoginUrl;
    fn submit_callback(
        &self,
        url: SensitiveLoginUrl,
    ) -> BoxFuture<'_, std::result::Result<(), LoginError>>;
    fn cancel(&self);
    fn wait(
        &self,
    ) -> BoxFuture<'_, std::result::Result<lattice_llm::auth::ProviderAuthStatus, LoginError>>;
}

impl LoginAttempt for ChatgptLogin {
    fn authorization_url(&self) -> &SensitiveLoginUrl {
        self.authorization_url()
    }
    fn submit_callback(
        &self,
        url: SensitiveLoginUrl,
    ) -> BoxFuture<'_, std::result::Result<(), LoginError>> {
        Box::pin(self.submit_callback(url))
    }
    fn cancel(&self) {
        self.cancel();
    }
    fn wait(
        &self,
    ) -> BoxFuture<'_, std::result::Result<lattice_llm::auth::ProviderAuthStatus, LoginError>> {
        Box::pin(self.wait())
    }
}

struct Input {
    messages: mpsc::Receiver<Result<Option<LoginAction>>>,
    reader: JoinHandle<()>,
}
impl Drop for Input {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn outcome(
    result: std::result::Result<
        (lattice_llm::auth::ProviderAuthStatus, AuthCatalogOutcome),
        LoginError,
    >,
) -> LoginResponse {
    match result {
        Ok((status, catalog)) => LoginResponse::AuthLoginResult(AuthLoginOutcome::SignedIn {
            status: crate::local::presentation::provider_auth_status(status),
            catalog,
        }),
        Err(LoginError::Cancelled) => {
            LoginResponse::AuthLoginResult(AuthLoginOutcome::Cancelled {})
        }
        Err(LoginError::Failed(error)) => {
            let kind = match &error {
                lattice_llm::LlmError::Config { .. } | lattice_llm::LlmError::Credential { .. } => {
                    LocalErrorKind::BadRequest
                }
                lattice_llm::LlmError::Transport { .. }
                | lattice_llm::LlmError::Dns { .. }
                | lattice_llm::LlmError::Timeout { .. }
                | lattice_llm::LlmError::StreamRead { .. } => LocalErrorKind::Transport,
                _ => LocalErrorKind::Internal,
            };
            let reason = crate::ConfigRejection::new(&error.to_string(), None);
            LoginResponse::Error(LocalError::new(kind, &reason.to_string()))
        }
    }
}

async fn finish(
    result: std::result::Result<lattice_llm::auth::ProviderAuthStatus, LoginError>,
    refresh: &impl Fn() -> BoxFuture<'static, AuthCatalogOutcome>,
) -> LoginResponse {
    let result = match result {
        Ok(status) => Ok((status, refresh().await)),
        Err(error) => Err(error),
    };
    outcome(result)
}

/// 认证规则归 LLM；这里只管理专用连接的输入、输出和取消责任。
pub(in crate::local) async fn exchange<A: LoginAttempt>(
    stream: UnixStream,
    start: impl std::future::Future<Output = std::result::Result<A, LoginError>>,
    refresh: impl Fn() -> BoxFuture<'static, AuthCatalogOutcome> + Send + Sync,
) -> Result<()> {
    let (mut read, mut write) = stream.into_split();
    let (sender, messages) = mpsc::channel(1);
    let reader = tokio::spawn(async move {
        loop {
            let message = ipc::read_login_action(&mut read).await;
            let finished = !matches!(&message, Ok(Some(LoginAction::Callback { .. })));
            if sender.send(message).await.is_err() || finished {
                break;
            }
        }
    });
    let mut input = Input { messages, reader };
    let mut writer = ipc::ResponseWriter::default();
    let mut start = Box::pin(start);
    let attempt = tokio::select! {
        biased;
        action = input.messages.recv() => {
            // 释放准备 future 会立即触发库内 CancelOnDrop，worker 由 manager 监督。
            drop(start);
            let response = match action {
                Some(Ok(None)) | None => return Ok(()),
                Some(Ok(Some(LoginAction::Cancel {}))) => LoginResponse::AuthLoginResult(AuthLoginOutcome::Cancelled {}),
                Some(Err(error)) => LoginResponse::Error(error),
                _ => LoginResponse::Error(protocol()),
            };
            return writer.write(&mut write, &response).await;
        }
        attempt = &mut start => match attempt {
            Ok(attempt) => attempt,
            Err(error) => return writer.write(&mut write, &outcome(Err(error))).await,
        },
    };
    let authorization = LoginResponse::AuthLoginAuthorization {
        authorization_url: AuthKey::new(attempt.authorization_url().expose().into()),
    };
    let written = tokio::select! {
        result = writer.write(&mut write, &authorization) => result,
        _ = input.messages.recv() => {
            // 授权帧可能只写了一部分，取消后关闭连接，不在半帧后追加另一条响应。
            attempt.cancel();
            let _ = attempt.wait().await;
            return Err(LocalError::new(LocalErrorKind::Transport, "login connection interrupted during authorization"));
        }
    };
    if let Err(error) = written {
        attempt.cancel();
        let _ = attempt.wait().await;
        return Err(error);
    }
    drop(authorization);
    let mut callback_sent = false;
    let response = loop {
        tokio::select! {
            biased;
            result = attempt.wait() => break finish(result, &refresh).await,
            action = input.messages.recv() => match action {
                Some(Ok(Some(LoginAction::Callback { mut url }))) if !callback_sent => {
                    callback_sent = true;
                    // 浏览器可能已先交付回调；仍以该尝试的最终结果为准。
                    let _ = attempt.submit_callback(SensitiveLoginUrl::new(url.take())).await;
                }
                Some(Ok(Some(LoginAction::Cancel {}))) => {
                    attempt.cancel();
                    break finish(attempt.wait().await, &refresh).await;
                }
                Some(Ok(None)) | None => {
                    attempt.cancel();
                    let _ = attempt.wait().await;
                    return Ok(());
                }
                message => {
                    attempt.cancel();
                    let final_result = attempt.wait().await;
                    if final_result.is_ok() { break finish(final_result, &refresh).await; }
                    break LoginResponse::Error(match message { Some(Err(error)) => error, _ => protocol() });
                }
            }
        }
    };
    input.reader.abort();
    let _ = (&mut input.reader).await;
    writer.write(&mut write, &response).await
}

#[cfg(test)]
mod tests;
