// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 前端登录流连接、敏感授权/回调 URL 封装与半帧读取任务。

use lattice_ipc::{
    AuthKey, AuthLoginOutcome, LocalError, LocalErrorKind, LoginAction, LoginResponse, Result, ipc,
};
use tokio::{
    net::{UnixStream, unix::OwnedWriteHalf},
    sync::mpsc,
    task::JoinHandle,
};
use zeroize::Zeroizing;

/// 含 OAuth `state` 或授权 `code` 的前端登录 URL；Drop 时清零缓冲，不实现隐式转换、格式化或序列化。
pub struct SensitiveLoginUrl(Zeroizing<String>);

impl SensitiveLoginUrl {
    pub fn new(url: String) -> Self {
        Self(Zeroizing::new(url))
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

/// 授权 URL 只在登录界面借用，事件本身不提供 Debug 或序列化。
pub enum AuthLoginEvent {
    Authorization(SensitiveLoginUrl),
    Finished(AuthLoginOutcome),
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

    fn from_reader(mut read: impl ipc::PeerStream + Send + 'static, write: OwnedWriteHalf) -> Self {
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
                                authorization_url.take(),
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
                url: AuthKey::new(url.expose().into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_ipc::{AuthCatalogOutcome, Availability, ProviderAuthStatus};
    use tokio::{
        io::AsyncWriteExt,
        time::{Duration, timeout},
    };

    const URL: &str =
        "https://auth.openai.com/authorize?state=private-state&id_token_hint=private-hint";

    fn signed_in() -> ProviderAuthStatus {
        ProviderAuthStatus {
            provider: "chatgpt".into(),
            availability: Availability::Ready,
            email: Some("user@example.com".into()),
            expires_at: None,
        }
    }

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        timeout(Duration::from_secs(3), future)
            .await
            .expect("登录客户端读取没有收尾")
    }

    async fn authorization(login: &mut LocalAuthLogin) {
        let Some(AuthLoginEvent::Authorization(url)) = bounded(login.next()).await.unwrap() else {
            panic!("授权事件缺失")
        };
        assert_eq!(url.expose(), URL);
    }

    #[tokio::test]
    async fn interrupted_ui_poll_keeps_partial_authorization_frame() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut login = LocalAuthLogin::from_stream(client);
        let payload = serde_json::to_vec(&serde_json::json!({
            "message_id": 0,
            "offset": 0,
            "complete": true,
            "json_fragment": serde_json::to_string(&LoginResponse::AuthLoginAuthorization {
                authorization_url: AuthKey::new(URL.into()),
            }).unwrap(),
        }))
        .unwrap();
        server.write_all(ipc::MAGIC).await.unwrap();
        server.write_u32(payload.len() as u32).await.unwrap();
        server.write_all(&payload[..7]).await.unwrap();
        for _ in 0..3 {
            assert!(
                timeout(Duration::from_millis(1), login.next())
                    .await
                    .is_err()
            );
        }
        server.write_all(&payload[7..]).await.unwrap();
        authorization(&mut login).await;
    }

    struct ObservedRead {
        read: tokio::net::unix::OwnedReadHalf,
        bytes: usize,
        waiting: Option<tokio::sync::oneshot::Sender<()>>,
        partial: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl ipc::PeerStream for ObservedRead {
        fn peer_fd(&self) -> Option<std::os::fd::BorrowedFd<'_>> {
            use std::os::fd::AsFd;
            Some(self.read.as_ref().as_fd())
        }
    }

    impl tokio::io::AsyncRead for ObservedRead {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            context: &mut std::task::Context<'_>,
            buffer: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            let before = buffer.filled().len();
            let result = std::pin::Pin::new(&mut self.read).poll_read(context, buffer);
            self.bytes += buffer.filled().len() - before;
            if result.is_pending() {
                let signal = if self.bytes == 0 {
                    self.waiting.take()
                } else {
                    self.partial.take()
                };
                if let Some(signal) = signal {
                    let _ = signal.send(());
                }
            }
            result
        }
    }

    fn observed_login(
        stream: UnixStream,
    ) -> (
        LocalAuthLogin,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (read, write) = stream.into_split();
        let (waiting, entered) = tokio::sync::oneshot::channel();
        let (partial, started) = tokio::sync::oneshot::channel();
        (
            LocalAuthLogin::from_reader(
                ObservedRead {
                    read,
                    bytes: 0,
                    waiting: Some(waiting),
                    partial: Some(partial),
                },
                write,
            ),
            entered,
            started,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn human_wait_remains_usable_after_the_reader_waits_over_300_seconds() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (mut login, waiting, _partial) = observed_login(client);
        waiting.await.unwrap();
        tokio::time::advance(Duration::from_secs(301)).await;
        let mut writer = ipc::ResponseWriter::default();
        writer
            .write(
                &mut server,
                &LoginResponse::AuthLoginAuthorization {
                    authorization_url: AuthKey::new(URL.into()),
                },
            )
            .await
            .unwrap();
        authorization(&mut login).await;
    }

    #[tokio::test(start_paused = true)]
    async fn partial_frame_times_out_ten_seconds_after_its_first_byte() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (mut login, waiting, partial) = observed_login(client);
        waiting.await.unwrap();
        server.write_all(b"L").await.unwrap();
        partial.await.unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        assert!(login.events.try_recv().is_err());
        tokio::time::advance(Duration::from_secs(2)).await;
        let error = login.next().await.err().unwrap();
        assert_eq!(error.kind, LocalErrorKind::Transport);
        assert!(error.message.contains("读取登录帧超时"));
    }

    #[tokio::test]
    async fn client_rejects_duplicate_authorization_and_signed_in_before_authorization() {
        for duplicate in [true, false] {
            let (client, mut server) = UnixStream::pair().unwrap();
            let mut login = LocalAuthLogin::from_stream(client);
            let mut writer = ipc::ResponseWriter::default();
            if duplicate {
                writer
                    .write(
                        &mut server,
                        &LoginResponse::AuthLoginAuthorization {
                            authorization_url: AuthKey::new(URL.into()),
                        },
                    )
                    .await
                    .unwrap();
                authorization(&mut login).await;
                writer
                    .write(
                        &mut server,
                        &LoginResponse::AuthLoginAuthorization {
                            authorization_url: AuthKey::new(URL.into()),
                        },
                    )
                    .await
                    .unwrap();
            } else {
                writer
                    .write(
                        &mut server,
                        &LoginResponse::AuthLoginResult(AuthLoginOutcome::SignedIn {
                            status: signed_in(),
                            catalog: AuthCatalogOutcome::Refreshed { model_count: 0 },
                        }),
                    )
                    .await
                    .unwrap();
            }
            let error = bounded(login.next()).await.err().unwrap();
            assert_eq!(error.kind, LocalErrorKind::Protocol);
        }
    }
}
