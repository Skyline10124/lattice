// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::AsyncWriteExt,
    sync::watch,
    time::{Duration, timeout},
};

const URL: &str =
    "https://auth.openai.com/authorize?state=private-state&id_token_hint=private-hint";

async fn exchange<A: LoginAttempt>(
    stream: UnixStream,
    start: impl std::future::Future<Output = std::result::Result<A, LoginError>>,
) -> Result<()> {
    super::exchange(stream, start, || {
        Box::pin(async { AuthCatalogOutcome::Refreshed { model_count: 0 } })
    })
    .await
}

use crate::local::{Availability, ConfigurationRequirement, ProviderAuthStatus};

struct Attempt {
    url: SensitiveLoginUrl,
    finished: watch::Sender<
        Option<std::result::Result<lattice_llm::auth::ProviderAuthStatus, LoginError>>,
    >,
    callbacks: mpsc::UnboundedSender<String>,
    cancelled: Arc<AtomicBool>,
}

fn llm_signed_in() -> lattice_llm::auth::ProviderAuthStatus {
    lattice_llm::auth::ProviderAuthStatus {
        provider: "chatgpt".into(),
        availability: lattice_llm::Availability::Ready,
        email: Some("user@example.com".into()),
        expires_at: None,
    }
}

fn signed_in() -> ProviderAuthStatus {
    ProviderAuthStatus {
        provider: "chatgpt".into(),
        availability: Availability::Ready,
        email: Some("user@example.com".into()),
        expires_at: None,
    }
}

impl LoginAttempt for Attempt {
    fn authorization_url(&self) -> &SensitiveLoginUrl {
        &self.url
    }
    fn submit_callback(
        &self,
        url: SensitiveLoginUrl,
    ) -> BoxFuture<'_, std::result::Result<(), LoginError>> {
        Box::pin(async move {
            self.callbacks.send(url.expose().into()).unwrap();
            Ok(())
        })
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.finished.send_if_modified(|result| {
            if result.is_none() {
                *result = Some(Err(LoginError::Cancelled));
                true
            } else {
                false
            }
        });
    }
    fn wait(
        &self,
    ) -> BoxFuture<'_, std::result::Result<lattice_llm::auth::ProviderAuthStatus, LoginError>> {
        let mut receiver = self.finished.subscribe();
        Box::pin(async move {
            receiver
                .wait_for(|result| result.is_some())
                .await
                .unwrap()
                .clone()
                .unwrap()
        })
    }
}

fn attempt() -> (Attempt, mpsc::UnboundedReceiver<String>) {
    let (callbacks, receiver) = mpsc::unbounded_channel();
    (
        Attempt {
            url: SensitiveLoginUrl::new(URL.into()),
            finished: watch::channel(None).0,
            callbacks,
            cancelled: Arc::default(),
        },
        receiver,
    )
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    timeout(Duration::from_secs(3), future)
        .await
        .expect("登录交换没有收尾")
}

async fn authorization(login: &mut LocalAuthLogin) {
    let Some(AuthLoginEvent::Authorization(url)) = bounded(login.next()).await.unwrap() else {
        panic!("授权事件缺失")
    };
    assert!(url.expose() == URL);
}

#[tokio::test]
async fn cancel_during_startup_drops_the_start_future_before_replying() {
    struct Cancelled(Arc<AtomicBool>);
    impl Drop for Cancelled {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (client, server) = UnixStream::pair().unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let control = cancelled.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let ready = entered.clone();
    let task = tokio::spawn(exchange(server, async move {
        let _guard = Cancelled(control);
        ready.notify_one();
        std::future::pending::<std::result::Result<Attempt, LoginError>>().await
    }));
    let mut login = LocalAuthLogin::from_stream(client);
    bounded(entered.notified()).await;
    login.cancel().await.unwrap();
    assert!(matches!(
        bounded(login.next()).await.unwrap(),
        Some(AuthLoginEvent::Finished(AuthLoginOutcome::Cancelled {}))
    ));
    bounded(task).await.unwrap().unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn cancellation_interrupts_a_blocked_authorization_write() {
    let (mut client, server) = UnixStream::pair().unwrap();
    let (mut attempt, _callbacks) = attempt();
    attempt.url = SensitiveLoginUrl::new("sensitive-authorization".repeat(65_536));
    let cancelled = attempt.cancelled.clone();
    let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
    bounded(client.readable()).await.unwrap();
    ipc::write(&mut client, &LoginAction::Cancel {})
        .await
        .unwrap();
    assert!(bounded(task).await.unwrap().is_err());
    assert!(cancelled.load(Ordering::SeqCst));
}

#[test]
fn login_failure_preserves_configuration_and_transport_classes() {
    for (error, expected) in [
        (
            lattice_llm::LlmError::Credential {
                message: "login required".into(),
            },
            LocalErrorKind::BadRequest,
        ),
        (
            lattice_llm::LlmError::Timeout {
                message: "token endpoint".into(),
            },
            LocalErrorKind::Transport,
        ),
    ] {
        let LoginResponse::Error(error) = outcome(Err(LoginError::Failed(error))) else {
            panic!("失败必须交付错误")
        };
        assert_eq!(error.kind, expected);
    }
}

#[test]
fn login_envelopes_payloads_and_outcomes_reject_unknown_fields() {
    for wire in [
        r#"{"response":"auth_login_authorization","value":{"authorization_url":"secret","token":"secret"}}"#,
        r#"{"response":"auth_login_authorization","value":{"authorization_url":"secret"},"token":"secret"}"#,
        r#"{"response":"auth_login_result","value":{"outcome":"cancelled","token":"secret"}}"#,
        r#"{"response":"auth_login_result","value":{"outcome":"signed_in","status":{"provider":"chatgpt","availability":{"status":"ready"},"token":"secret"},"catalog":{"status":"refreshed","model_count":0}}}"#,
    ] {
        assert!(serde_json::from_str::<LoginResponse>(wire).is_err());
    }
}

#[tokio::test]
async fn one_socket_delivers_authorization_callback_and_final_result() {
    let (client, server) = UnixStream::pair().unwrap();
    let (attempt, mut callbacks) = attempt();
    let finished = attempt.finished.clone();
    let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
    let mut login = LocalAuthLogin::from_stream(client);
    authorization(&mut login).await;
    login
        .submit_callback(SensitiveLoginUrl::new(
            "http://127.0.0.1:1455/auth/callback?code=private-code".into(),
        ))
        .await
        .unwrap();
    assert!(
        bounded(callbacks.recv())
            .await
            .unwrap()
            .contains("private-code")
    );
    finished.send_replace(Some(Ok(llm_signed_in())));
    assert!(matches!(
        bounded(login.next()).await.unwrap(),
        Some(AuthLoginEvent::Finished(AuthLoginOutcome::SignedIn { .. }))
    ));
    bounded(task).await.unwrap().unwrap();
    assert!(login.next().await.unwrap().is_none());
}

#[tokio::test]
async fn browser_completion_needs_no_frontend_message_and_wins_late_cancel() {
    let (client, server) = UnixStream::pair().unwrap();
    let (attempt, _callbacks) = attempt();
    let finished = attempt.finished.clone();
    let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
    let mut login = LocalAuthLogin::from_stream(client);
    authorization(&mut login).await;
    finished.send_replace(Some(Ok(llm_signed_in())));
    let _ = login.cancel().await;
    assert!(matches!(
        bounded(login.next()).await.unwrap(),
        Some(AuthLoginEvent::Finished(AuthLoginOutcome::SignedIn { .. }))
    ));
    bounded(task).await.unwrap().unwrap();
}

#[tokio::test]
async fn saved_login_waits_for_the_directory_result_and_directory_failure_keeps_it_signed_in() {
    for expected in [
        AuthCatalogOutcome::Refreshed { model_count: 0 },
        AuthCatalogOutcome::Refreshed { model_count: 2 },
        AuthCatalogOutcome::Failed {
            warning: "directory unavailable".into(),
        },
    ] {
        let (client, server) = UnixStream::pair().unwrap();
        let (attempt, _callbacks) = attempt();
        let saved = attempt.finished.clone();
        let cancelled = attempt.cancelled.clone();
        let (entered, mut calls) = mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let result = expected.clone();
        let task = tokio::spawn(super::exchange(server, async { Ok(attempt) }, {
            let release = release.clone();
            move || {
                entered.send(()).unwrap();
                let release = release.clone();
                let result = result.clone();
                Box::pin(async move {
                    release.acquire().await.unwrap().forget();
                    result
                })
            }
        }));
        let mut login = LocalAuthLogin::from_stream(client);
        authorization(&mut login).await;
        assert!(calls.try_recv().is_err(), "保存成功之前不刷新目录");
        saved.send_replace(Some(Ok(llm_signed_in())));
        bounded(calls.recv()).await.unwrap();
        assert!(
            timeout(Duration::from_millis(1), login.next())
                .await
                .is_err()
        );
        release.add_permits(1);
        let Some(AuthLoginEvent::Finished(AuthLoginOutcome::SignedIn { status, catalog })) =
            bounded(login.next()).await.unwrap()
        else {
            panic!("目录失败不能撤销已保存的登录");
        };
        assert_eq!(status, signed_in());
        assert_eq!(catalog, expected);
        assert!(!cancelled.load(Ordering::SeqCst));
        bounded(task).await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn cancel_eof_and_partial_frame_eof_cancel_the_attempt() {
    for mode in ["cancel", "eof", "partial"] {
        let (client, server) = UnixStream::pair().unwrap();
        let (attempt, _callbacks) = attempt();
        let cancelled = attempt.cancelled.clone();
        let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
        let mut login = LocalAuthLogin::from_stream(client);
        authorization(&mut login).await;
        if mode == "cancel" {
            login.cancel().await.unwrap();
            assert!(matches!(
                bounded(login.next()).await.unwrap(),
                Some(AuthLoginEvent::Finished(AuthLoginOutcome::Cancelled {}))
            ));
        } else {
            if mode == "partial" {
                login.write.write_all(b"LTL").await.unwrap();
            }
            drop(login);
        }
        let _ = bounded(task).await.unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn unknown_fields_and_wrong_actions_fail_without_echoing_material() {
    for action in [
        r#"{"action":"cancel","token":"private-extra"}"#,
        r#"{"action":"callback","url":"private-url","key":"private-extra"}"#,
        r#"{"action":"unknown","url":"private-url"}"#,
        r#"{"action":"callback","url":"one","url":"two"}"#,
    ] {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (attempt, _callbacks) = attempt();
        let cancelled = attempt.cancelled.clone();
        let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
        let mut reader = ipc::ResponseReader::for_login();
        assert!(matches!(
            reader.read::<LoginResponse>(&mut client).await.unwrap(),
            LoginResponse::AuthLoginAuthorization { .. }
        ));
        client.write_all(ipc::MAGIC).await.unwrap();
        client.write_u32(action.len() as u32).await.unwrap();
        client.write_all(action.as_bytes()).await.unwrap();
        let LoginResponse::Error(error) = bounded(reader.read(&mut client)).await.unwrap() else {
            panic!("必须拒绝畸形消息")
        };
        assert_eq!(error.kind, LocalErrorKind::Protocol);
        assert!(!error.message.contains("private"));
        bounded(task).await.unwrap().unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn interrupted_ui_poll_keeps_partial_authorization_frame() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let mut login = LocalAuthLogin::from_stream(client);
    let payload = serde_json::to_vec(&serde_json::json!({"message_id":0,"offset":0,"complete":true,
        "json_fragment":serde_json::to_string(&LoginResponse::AuthLoginAuthorization { authorization_url: AuthKey::new(URL.into()) }).unwrap()})).unwrap();
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
    assert!(error.message.contains("timed out"));
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

#[tokio::test]
async fn second_legal_callback_cancels_attempt_and_is_not_delivered() {
    let (client, server) = UnixStream::pair().unwrap();
    let (attempt, mut callbacks) = attempt();
    let cancelled = attempt.cancelled.clone();
    let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
    let mut login = LocalAuthLogin::from_stream(client);
    authorization(&mut login).await;
    login
        .submit_callback(SensitiveLoginUrl::new("http://callback?code=first".into()))
        .await
        .unwrap();
    assert_eq!(
        bounded(callbacks.recv()).await.unwrap(),
        "http://callback?code=first"
    );
    login
        .submit_callback(SensitiveLoginUrl::new("http://callback?code=second".into()))
        .await
        .unwrap();
    let error = bounded(login.next()).await.err().unwrap();
    assert_eq!(error.kind, LocalErrorKind::Protocol);
    bounded(task).await.unwrap().unwrap();
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(callbacks.try_recv().is_err());
}

#[tokio::test]
async fn login_success_frame_converts_provider_auth_status_variants_and_fields() {
    for (llm_status, expected_status) in [
        (
            lattice_llm::auth::ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: lattice_llm::Availability::Ready,
                email: Some("dev@example.com".into()),
                expires_at: Some("2026-12-01T00:00:00Z".into()),
            },
            ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: Availability::Ready,
                email: Some("dev@example.com".into()),
                expires_at: Some("2026-12-01T00:00:00Z".into()),
            },
        ),
        (
            lattice_llm::auth::ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: lattice_llm::Availability::Unconfigured {
                    requirement: lattice_llm::ConfigurationRequirement::ChatgptLogin,
                },
                email: None,
                expires_at: None,
            },
            ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: Availability::Unconfigured {
                    requirement: ConfigurationRequirement::ChatgptLogin,
                },
                email: None,
                expires_at: None,
            },
        ),
        (
            lattice_llm::auth::ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: lattice_llm::Availability::Invalid {
                    reason: "expired token".into(),
                },
                email: Some("stale@example.com".into()),
                expires_at: None,
            },
            ProviderAuthStatus {
                provider: "chatgpt".into(),
                availability: Availability::Invalid {
                    reason: "expired token".into(),
                },
                email: Some("stale@example.com".into()),
                expires_at: None,
            },
        ),
    ] {
        let (client, server) = UnixStream::pair().unwrap();
        let (attempt, _callbacks) = attempt();
        let finished = attempt.finished.clone();
        let task = tokio::spawn(exchange(server, async { Ok(attempt) }));
        let mut login = LocalAuthLogin::from_stream(client);
        authorization(&mut login).await;
        finished.send_replace(Some(Ok(llm_status)));
        let Some(AuthLoginEvent::Finished(AuthLoginOutcome::SignedIn { status, catalog })) =
            bounded(login.next()).await.unwrap()
        else {
            panic!("登录成功帧必须返回转换后的 ProviderAuthStatus");
        };
        assert_eq!(status, expected_status);
        assert_eq!(catalog, AuthCatalogOutcome::Refreshed { model_count: 0 });
        bounded(task).await.unwrap().unwrap();
    }
}
