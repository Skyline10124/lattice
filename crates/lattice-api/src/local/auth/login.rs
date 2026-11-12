// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use futures::future::BoxFuture;
use lattice_llm::auth::{ChatgptLogin, LoginError};
use serde::{Deserialize, Serialize};
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::mpsc,
    task::JoinHandle,
};

use super::AuthKey;
use crate::local::{
    LocalError, LocalErrorKind, ProviderAuthStatus, Result, SensitiveLoginUrl, ipc,
};

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

/// 授权 URL 只在登录界面借用，事件本身不提供 Debug 或序列化。
pub enum AuthLoginEvent {
    Authorization(SensitiveLoginUrl),
    Finished(AuthLoginOutcome),
}

#[derive(Serialize, Deserialize)]
#[serde(
    tag = "response",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(in crate::local) enum LoginResponse {
    AuthLoginAuthorization { authorization_url: AuthKey },
    AuthLoginResult(AuthLoginOutcome),
    Error(LocalError),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(in crate::local) enum LoginAction {
    Callback { url: AuthKey },
    Cancel {},
}

/// 单次登录独占一个连接；持续读任务保存分片进度，Drop 关闭前端连接。
pub struct LocalAuthLogin {
    write: OwnedWriteHalf,
    events: mpsc::Receiver<Result<AuthLoginEvent>>,
    reader: JoinHandle<()>,
}

impl LocalAuthLogin {
    pub(crate) fn from_stream(stream: UnixStream) -> Self {
        let (read, write) = stream.into_split();
        Self::from_reader(read, write)
    }

    fn from_reader(
        mut read: impl tokio::io::AsyncRead + Unpin + Send + 'static,
        write: OwnedWriteHalf,
    ) -> Self {
        let (sender, events) = mpsc::channel(2);
        let reader = tokio::spawn(async move {
            let mut frames = ipc::ResponseReader::for_login();
            let mut authorized = false;
            loop {
                let (event, done) = match frames.read::<LoginResponse>(&mut read).await {
                    Ok(LoginResponse::AuthLoginAuthorization {
                        mut authorization_url,
                    }) if !authorized => {
                        authorized = true;
                        (
                            Ok(AuthLoginEvent::Authorization(SensitiveLoginUrl::new(
                                std::mem::take(&mut authorization_url.0),
                            ))),
                            false,
                        )
                    }
                    Ok(LoginResponse::AuthLoginResult(result))
                        if authorized || matches!(result, AuthLoginOutcome::Cancelled {}) =>
                    {
                        (Ok(AuthLoginEvent::Finished(result)), true)
                    }
                    Ok(LoginResponse::Error(error)) | Err(error) => (Err(error), true),
                    _ => (Err(protocol()), true),
                };
                if sender.send(event).await.is_err() || done {
                    break;
                }
            }
        });
        Self {
            write,
            events,
            reader,
        }
    }

    pub async fn next(&mut self) -> Result<Option<AuthLoginEvent>> {
        self.events.recv().await.transpose()
    }

    pub async fn submit_callback(&mut self, url: SensitiveLoginUrl) -> Result<()> {
        ipc::write(
            &mut self.write,
            &LoginAction::Callback {
                url: AuthKey(url.expose().into()),
            },
        )
        .await
    }

    pub async fn cancel(&mut self) -> Result<()> {
        ipc::write(&mut self.write, &LoginAction::Cancel {}).await
    }
}

impl Drop for LocalAuthLogin {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

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
    fn wait(&self) -> BoxFuture<'_, std::result::Result<ProviderAuthStatus, LoginError>>;
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
    fn wait(&self) -> BoxFuture<'_, std::result::Result<ProviderAuthStatus, LoginError>> {
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
    result: std::result::Result<(ProviderAuthStatus, AuthCatalogOutcome), LoginError>,
) -> LoginResponse {
    match result {
        Ok((status, catalog)) => {
            LoginResponse::AuthLoginResult(AuthLoginOutcome::SignedIn { status, catalog })
        }
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
    result: std::result::Result<ProviderAuthStatus, LoginError>,
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
        authorization_url: AuthKey(attempt.authorization_url().expose().into()),
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
                    let _ = attempt.submit_callback(SensitiveLoginUrl::new(std::mem::take(&mut url.0))).await;
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
