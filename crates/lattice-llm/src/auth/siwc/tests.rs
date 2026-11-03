// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::auth::{
    AuthState, Secret,
    oauth::TokenReply,
    tests::{chatgpt, fixture, write},
};
use futures::{
    future::{BoxFuture, join_all},
    pin_mut, poll,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicI64, AtomicUsize, Ordering},
};
use tokio::sync::Semaphore;

#[derive(Clone)]
enum Outcome {
    Success,
    Invalid(&'static str),
    Failure(LlmError),
}

struct Endpoint {
    now: Arc<AtomicI64>,
    calls: AtomicUsize,
    started: Semaphore,
    release: Semaphore,
    outcome: Mutex<Outcome>,
    revoked: Mutex<Vec<String>>,
    revoke_result: Mutex<Result<(), LlmError>>,
    revoke_started: Semaphore,
    revoke_gate: Mutex<Option<Arc<Semaphore>>>,
}

impl TokenEndpoint for Endpoint {
    fn exchange<'a>(
        &'a self,
        _: super::super::oauth::CodeGrant<'a>,
    ) -> BoxFuture<'a, Result<super::super::oauth::LoginTokens, super::super::oauth::ExchangeFailure>>
    {
        unreachable!("刷新测试不执行登录换码")
    }
    fn discovery(&self) -> BoxFuture<'_, Result<url::Url, LlmError>> {
        unreachable!("刷新测试不查询登录密钥")
    }
    fn jwks<'a>(
        &'a self,
        _: &'a url::Url,
    ) -> BoxFuture<'a, Result<jsonwebtoken::jwk::JwkSet, LlmError>> {
        unreachable!("刷新测试不查询登录密钥")
    }

    fn refresh<'a>(
        &'a self,
        grant: RefreshGrant<'a>,
    ) -> BoxFuture<'a, Result<TokenReply, RefreshFailure>> {
        Box::pin(async move {
            assert!(grant.client_id == "oaiapp_test-issued-client");
            assert!(grant.refresh_token == "refresh-test-secret");
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.add_permits(1);
            tokio::time::timeout(std::time::Duration::from_secs(5), self.release.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            match self.outcome.lock().unwrap().clone() {
                Outcome::Success => Ok(TokenReply {
                    access_token: Secret("rotated-access-secret".into()),
                    refresh_token: Secret("rotated-refresh-secret".into()),
                    expires_at: (chrono::DateTime::from_timestamp(
                        self.now.load(Ordering::SeqCst),
                        0,
                    )
                    .unwrap()
                        + TimeDelta::hours(1))
                    .to_rfc3339(),
                    scopes: vec![DIRECT_SCOPE.into(), "offline_access".into()],
                }),
                Outcome::Invalid(code) => Err(RefreshFailure::InvalidToken(code)),
                Outcome::Failure(error) => Err(RefreshFailure::Other(error)),
            }
        })
    }

    fn revoke<'a>(&'a self, grant: RefreshGrant<'a>) -> BoxFuture<'a, Result<(), LlmError>> {
        Box::pin(async move {
            assert!(grant.client_id == "oaiapp_test-issued-client");
            self.revoked
                .lock()
                .unwrap()
                .push(grant.refresh_token.into());
            self.revoke_started.add_permits(1);
            let gate = self.revoke_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                tokio::time::timeout(std::time::Duration::from_secs(5), gate.acquire())
                    .await
                    .unwrap()
                    .unwrap()
                    .forget();
            }
            self.revoke_result.lock().unwrap().clone()
        })
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: AuthStore,
    auth: ChatgptAuth,
    endpoint: Arc<Endpoint>,
    now: Arc<AtomicI64>,
}

#[tokio::test]
async fn account_directory_uses_refreshed_token_and_publishes_empty_and_nonempty_results() {
    use crate::metadata::{MetadataRefreshControl, chatgpt};
    let fixture = Fixture::new(299);
    let refresh = chatgpt::refresh_with(
        &fixture.store.directory,
        &fixture.auth,
        Arc::default(),
        |header| async move {
            assert!(header.is_sensitive());
            assert!(header == "Bearer rotated-access-secret");
            Ok(vec![])
        },
    );
    tokio::pin!(refresh);
    assert!(poll!(refresh.as_mut()).is_pending());
    fixture.started().await;
    assert!(
        !fixture
            .store
            .directory
            .join("cache/chatgpt-models.json")
            .exists()
    );
    fixture.endpoint.release.add_permits(1);
    assert_eq!(refresh.await.unwrap(), 0);
    assert_eq!(fixture.endpoint.calls.load(Ordering::SeqCst), 1);
    let state = chatgpt::state(&fixture.store.directory).unwrap();
    assert!(state.logged_in);
    assert_eq!(state.model_count, Some(0));
    assert!(state.fetched_at.is_some());
    let count = chatgpt::refresh_with(
        &fixture.store.directory,
        &fixture.auth,
        Arc::default(),
        |_| async {
            Ok(vec![
                chatgpt::AccountModel {
                    slug: "first-model".into(),
                    display_name: "First model".into(),
                },
                chatgpt::AccountModel {
                    slug: "second-model".into(),
                    display_name: "Second model".into(),
                },
            ])
        },
    )
    .await
    .unwrap();
    assert_eq!(count, 2);
    assert_eq!(
        chatgpt::state(&fixture.store.directory)
            .unwrap()
            .model_count,
        Some(2)
    );
    let previous = fs::read(fixture.store.directory.join("cache/chatgpt-models.json")).unwrap();
    let published: serde_json::Value = serde_json::from_slice(&previous).unwrap();
    assert_eq!(
        published["models"],
        serde_json::json!([
            {"slug":"first-model","display_name":"First model"},
            {"slug":"second-model","display_name":"Second model"}
        ])
    );
    let cancel = Arc::new(MetadataRefreshControl::default());
    let cancel_request = cancel.clone();
    let error = chatgpt::refresh_with(
        &fixture.store.directory,
        &fixture.auth,
        cancel,
        |_| async move {
            cancel_request.cancel();
            Ok(vec![chatgpt::AccountModel {
                slug: "new-model".into(),
                display_name: "New model".into(),
            }])
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, crate::MetadataError::Cancelled));
    assert_eq!(
        fs::read(fixture.store.directory.join("cache/chatgpt-models.json")).unwrap(),
        previous
    );
}

#[tokio::test]
async fn account_directory_never_calls_remote_without_a_usable_token() {
    use crate::metadata::chatgpt;
    let fixture = Fixture::new(299);
    *fixture.endpoint.outcome.lock().unwrap() = Outcome::Invalid("invalid_grant");
    fixture.endpoint.release.add_permits(1);
    for expected_refreshes in [1, 1] {
        let error = chatgpt::refresh_with(
            &fixture.store.directory,
            &fixture.auth,
            Arc::default(),
            |_| async { panic!("没有可用 token 时不能调用目录 endpoint") },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            crate::MetadataError::Chatgpt(LlmError::Credential { .. })
        ));
        assert_eq!(
            fixture.endpoint.calls.load(Ordering::SeqCst),
            expected_refreshes
        );
    }
}

#[tokio::test]
async fn account_directory_response_cannot_publish_after_remove_or_logout() {
    use crate::metadata::chatgpt;
    for remove in [false, true] {
        let fixture = Fixture::new(3600);
        let path = fixture.store.directory.join("cache/chatgpt-models.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"previous cache").unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        let auth = fixture.auth.clone();
        let directory = fixture.store.directory.clone();
        let task = tokio::spawn(async move {
            chatgpt::refresh_with(&directory, &auth, Arc::default(), |_| async move {
                started.send(()).unwrap();
                released.await.unwrap();
                Ok(vec![chatgpt::AccountModel {
                    slug: "old-account-model".into(),
                    display_name: "Old account".into(),
                }])
            })
            .await
        });
        ready.await.unwrap();
        if remove {
            fixture.auth.remove().await.unwrap();
        } else {
            fixture.auth.logout().await.unwrap();
        }
        release.send(()).unwrap();
        assert!(matches!(
            task.await.unwrap(),
            Err(crate::MetadataError::Chatgpt(LlmError::Credential { .. }))
        ));
        if remove {
            assert!(!path.exists());
        } else {
            assert_eq!(fs::read(path).unwrap(), b"previous cache");
        }
    }
}

impl Fixture {
    fn new(remaining: i64) -> Self {
        let (directory, store) = fixture();
        let now = Arc::new(AtomicI64::new(1_800_000_000));
        let endpoint = Arc::new(Endpoint {
            now: now.clone(),
            calls: AtomicUsize::new(0),
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            outcome: Mutex::new(Outcome::Success),
            revoked: Mutex::new(vec![]),
            revoke_result: Mutex::new(Ok(())),
            revoke_started: Semaphore::new(0),
            revoke_gate: Mutex::new(None),
        });
        let clock_value = now.clone();
        let auth = ChatgptAuth::with_dependencies(
            store.clone(),
            endpoint.clone(),
            Arc::new(move || {
                chrono::DateTime::from_timestamp(clock_value.load(Ordering::SeqCst), 0).unwrap()
            }),
        );
        let result = Self {
            _directory: directory,
            store,
            auth,
            endpoint,
            now,
        };
        result.save(remaining);
        result
    }

    fn save(&self, remaining: i64) {
        let mut entry = chatgpt();
        entry["client_id"] = serde_json::json!("oaiapp_test-issued-client");
        entry["tokens"]["expires_at"] = serde_json::json!(
            chrono::DateTime::from_timestamp(self.now.load(Ordering::SeqCst) + remaining, 0)
                .unwrap()
                .to_rfc3339()
        );
        write(
            &self.store,
            serde_json::json!({"version":1,"providers":{"chatgpt":entry}}).to_string(),
        );
    }

    fn document(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.store.directory.join("auth.json")).unwrap()).unwrap()
    }

    async fn started(&self) {
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.endpoint.started.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
    }

    fn start(&self) -> tokio::task::JoinHandle<Result<AccessToken, LlmError>> {
        let auth = self.auth.clone();
        tokio::spawn(async move { auth.access_token().await })
    }
}

#[tokio::test]
async fn stopping_logins_preserves_token_use_and_refresh_until_shutdown() {
    let f = Fixture::new(3600);
    f.auth.stop_logins();
    assert!(f.auth.access_token().await.is_ok());
    f.save(299);
    f.endpoint.release.add_permits(1);
    let token = f.auth.access_token().await.unwrap();
    assert_eq!(
        token.authorization().unwrap(),
        "Bearer rotated-access-secret"
    );
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
    f.auth.shutdown().await.unwrap();
    assert!(f.auth.access_token().await.is_err());
}

#[tokio::test]
async fn inference_waits_for_refresh_publication_and_uses_its_header_and_redaction() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new(-1);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let auth = f.auth.clone();
    let credentials = Arc::new(f.store.load().unwrap());
    let send = tokio::spawn(async move {
        let mut request =
            crate::ChatRequest::new(vec![], crate::ModelTarget::new("chatgpt", "model"));
        request.client_request_id = Some("session".into());
        crate::client::dispatch(
            request,
            crate::client::RequestEndpoint {
                provider: "chatgpt",
                model: "model",
                base_url: &url,
                protocol: crate::protocol::Protocol::Responses,
                auth: crate::provider::Auth::OauthChatgpt,
                credentials,
                chatgpt_auth: &auth,
                roots: &[],
                client: Arc::default(),
                output_limit: None,
                model_reasoning: None,
                features: crate::protocol::Features::Responses,
                reasoning: None,
            },
            crate::StreamControl::default(),
        )
        .await
    });
    f.started().await;
    assert!(poll!(Box::pin(listener.accept())).is_pending());
    f.endpoint.release.add_permits(1);
    let (mut wire, _) = tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(wire.read_u8().await.unwrap());
    }
    let head = String::from_utf8(head).unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("authorization: bearer rotated-access-secret")
    );
    assert_eq!(
        f.document()["providers"]["chatgpt"]["tokens"]["access_token"],
        "rotated-access-secret"
    );
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|length| length.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    wire.read_exact(&mut vec![0; length]).await.unwrap();
    let body = r#"{"error":{"message":"rotated-access-secret rotated-refresh-secret"}}"#;
    wire.write_all(
        format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let error = send.await.unwrap().err().unwrap();
    assert!(!error.to_string().contains("rotated-access-secret"));
    assert!(!error.to_string().contains("rotated-refresh-secret"));
    assert!(error.to_string().contains("[REDACTED]"));
}

#[tokio::test]
async fn access_checks_exact_margin_and_uses_injected_clock() {
    for remaining in [301, 300, 299, 0, -1] {
        let f = Fixture::new(remaining);
        f.endpoint.release.add_permits(1);
        let token = f.auth.access_token().await.unwrap();
        let refreshes = usize::from(remaining < 300);
        assert_eq!(
            f.endpoint.calls.load(Ordering::SeqCst),
            refreshes,
            "{remaining}"
        );
        let header = token.authorization().unwrap();
        assert!(header.is_sensitive());
        assert!(!format!("{header:?}").contains("secret"));
        assert!(
            header
                == if refreshes == 0 {
                    "Bearer access-test-secret"
                } else {
                    "Bearer rotated-access-secret"
                }
        );
        if refreshes != 0 {
            let diagnostic = token
                .credentials
                .redact("rotated-access-secret rotated-refresh-secret id-test-secret");
            assert!(!diagnostic.contains("secret"));
            let saved = f.document();
            let tokens = &saved["providers"]["chatgpt"]["tokens"];
            assert!(tokens["id_token"] == "id-test-secret");
            assert!(tokens["refresh_token"] == "rotated-refresh-secret");
            assert_eq!(tokens["expires_at"], "2027-01-15T09:00:00+00:00");
            assert_eq!(
                tokens["scopes"],
                serde_json::json!([DIRECT_SCOPE, "offline_access"])
            );
            let next = f.auth.access_token().await.unwrap();
            assert!(next.authorization().unwrap() == "Bearer rotated-access-secret");
            assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), refreshes);
        }
    }
    let f = Fixture::new(301);
    f.auth.access_token().await.unwrap();
    f.now.fetch_add(2, Ordering::SeqCst);
    f.endpoint.release.add_permits(1);
    f.auth.access_token().await.unwrap();
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_callers_share_success_and_each_classified_failure() {
    let mut outcomes = vec![Outcome::Success];
    outcomes.extend(
        [
            "invalid_grant",
            "invalid_refresh_token",
            "token_expired",
            "refresh_token_expired",
            "refresh_token_invalidated",
            "refresh_token_reused",
        ]
        .map(Outcome::Invalid),
    );
    outcomes.extend([
        Outcome::Failure(LlmError::ProviderHttp {
            status_code: 400,
            message: "invalid_client".into(),
            retryable: false,
            retry_after: None,
        }),
        Outcome::Failure(LlmError::Transport {
            message: "refresh network failure".into(),
            retryable: true,
        }),
        Outcome::Failure(LlmError::Timeout {
            message: "refresh timeout".into(),
        }),
        Outcome::Failure(LlmError::ProviderHttp {
            status_code: 503,
            message: "refresh unavailable".into(),
            retryable: true,
            retry_after: Some(std::time::Duration::from_secs(3)),
        }),
    ]);
    for outcome in outcomes {
        let f = Fixture::new(299);
        let before = f.document();
        *f.endpoint.outcome.lock().unwrap() = outcome.clone();
        let callers = join_all((0..8).map(|_| f.auth.access_token()));
        pin_mut!(callers);
        assert!(poll!(callers.as_mut()).is_pending());
        f.started().await;
        f.endpoint.release.add_permits(1);
        let results = callers.await;
        assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
        match outcome {
            Outcome::Success => {
                assert!(results.iter().all(Result::is_ok));
                assert!(
                    f.document()["providers"]["chatgpt"]["tokens"]["access_token"]
                        == "rotated-access-secret"
                );
            }
            Outcome::Invalid(_) => {
                let errors: Vec<_> = results
                    .into_iter()
                    .map(|result| result.err().unwrap())
                    .collect();
                assert!(errors.iter().all(|error| *error == errors[0]
                    && !error.retryable()
                    && matches!(error, LlmError::Credential { .. })));
                let saved = f.document();
                assert!(saved["providers"]["chatgpt"].get("tokens").is_none());
                for field in ["host_id", "client_id", "account"] {
                    assert!(
                        saved["providers"]["chatgpt"][field]
                            == before["providers"]["chatgpt"][field]
                    );
                }
            }
            Outcome::Failure(expected) => {
                assert!(
                    results
                        .into_iter()
                        .all(|result| result.err().unwrap() == expected)
                );
                assert!(f.document() == before);
                *f.endpoint.outcome.lock().unwrap() = Outcome::Success;
                f.endpoint.release.add_permits(1);
                f.auth.access_token().await.unwrap();
                assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 2);
            }
        }
    }
}

#[tokio::test]
async fn cancellation_of_one_or_all_waiters_does_not_cancel_rotation() {
    for all in [false, true] {
        let f = Fixture::new(299);
        let first = f.start();
        f.started().await;
        let second = f.auth.access_token();
        pin_mut!(second);
        if !all {
            assert!(poll!(second.as_mut()).is_pending());
        }
        first.abort();
        let shutdown = f.auth.shutdown();
        pin_mut!(shutdown);
        if all {
            assert!(poll!(shutdown.as_mut()).is_pending());
        }
        f.endpoint.release.add_permits(1);
        if all {
            shutdown.await.unwrap();
        } else {
            second.await.unwrap();
            f.auth.shutdown().await.unwrap();
        }
        assert!(
            f.document()["providers"]["chatgpt"]["tokens"]["refresh_token"]
                == "rotated-refresh-secret"
        );
        assert!(f.auth.access_token().await.is_err());
        assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn logout_and_remove_wait_for_rotation_and_revoke_the_latest_token() {
    for remove in [false, true] {
        let f = Fixture::new(299);
        let first = f.start();
        f.started().await;
        let cleanup = f.auth.clean(remove);
        pin_mut!(cleanup);
        assert!(poll!(cleanup.as_mut()).is_pending());
        assert!(f.auth.access_token().await.is_err());
        f.endpoint.release.add_permits(1);
        let result = cleanup.await.unwrap();
        assert_eq!(result.revocation, RevocationStatus::Confirmed);
        assert!(first.await.unwrap().is_err());
        assert!(f.endpoint.revoked.lock().unwrap().as_slice() == ["rotated-refresh-secret"]);
        let saved = f.document();
        let entry = &saved["providers"]["chatgpt"];
        assert!(entry.get("tokens").is_none());
        assert!(entry["host_id"] == "host");
        assert_eq!(entry.get("client_id").is_none(), remove);
        assert_eq!(entry.get("account").is_none(), remove);
        f.auth.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn refresh_preserves_parallel_api_key_updates_and_rejects_replaced_registration() {
    for replace in [false, true] {
        let f = Fixture::new(299);
        let request = f.start();
        f.started().await;
        if replace {
            let mut entry = chatgpt();
            entry["host_id"] = serde_json::json!("different-host");
            entry["client_id"] = serde_json::json!("oaiapp_replacement-client");
            entry["account"]["subject"] = serde_json::json!("replacement-subject");
            entry["tokens"]["access_token"] = serde_json::json!("replacement-access-secret");
            entry["tokens"]["refresh_token"] = serde_json::json!("replacement-refresh-secret");
            entry["tokens"]["id_token"] = serde_json::json!("replacement-id-secret");
            entry["tokens"]["expires_at"] = serde_json::json!(
                chrono::DateTime::from_timestamp(f.now.load(Ordering::SeqCst) + 3600, 0)
                    .unwrap()
                    .to_rfc3339()
            );
            write(
                &f.store,
                serde_json::json!({"version":1,"providers":{"chatgpt":entry}}).to_string(),
            );
        } else {
            f.store.set("deepseek", "parallel-key-secret").unwrap();
        }
        f.endpoint.release.add_permits(1);
        let result = request.await.unwrap();
        let saved = f.document();
        if replace {
            assert!(result.is_err());
            assert!(saved["providers"]["chatgpt"]["host_id"] == "different-host");
            assert!(
                saved["providers"]["chatgpt"]["tokens"]["access_token"]
                    == "replacement-access-secret"
            );
            let token = f.auth.access_token().await.unwrap();
            assert!(token.authorization().unwrap() == "Bearer replacement-access-secret");
            assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
            assert!(f.document() == saved);
        } else {
            assert!(result.is_ok());
            assert!(saved["providers"]["deepseek"]["key"] == "parallel-key-secret");
        }
    }
}

#[tokio::test]
async fn cleanup_conflicts_do_not_block_replaced_credentials() {
    for remove in [false, true] {
        let f = Fixture::new(3600);
        let gate = Arc::new(Semaphore::new(0));
        *f.endpoint.revoke_gate.lock().unwrap() = Some(gate.clone());
        let auth = f.auth.clone();
        let cleanup = tokio::spawn(async move {
            if remove {
                auth.remove().await.map(|_| ())
            } else {
                auth.logout().await.map(|_| ())
            }
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.endpoint.revoke_started.acquire(),
        )
        .await
        .unwrap()
        .unwrap()
        .forget();
        let mut replacement = f.document();
        replacement["providers"]["chatgpt"]["tokens"]["access_token"] =
            serde_json::json!("replacement-access-secret");
        replacement["providers"]["chatgpt"]["tokens"]["refresh_token"] =
            serde_json::json!("replacement-refresh-secret");
        replacement["providers"]["chatgpt"]["tokens"]["id_token"] =
            serde_json::json!("replacement-id-secret");
        write(&f.store, replacement.to_string());
        gate.add_permits(1);
        assert!(cleanup.await.unwrap().is_err());
        let token = f.auth.access_token().await.unwrap();
        assert!(token.authorization().unwrap() == "Bearer replacement-access-secret");
        assert!(f.document() == replacement);
        assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn write_failure_stops_use_and_does_not_reconsume_a_rotated_refresh_token() {
    let f = Fixture::new(299);
    let before = f.document();
    let request = f.start();
    f.started().await;
    fs::set_permissions(&f.store.directory, fs::Permissions::from_mode(0o500)).unwrap();
    f.endpoint.release.add_permits(1);
    let result = request.await.unwrap();
    fs::set_permissions(&f.store.directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result, Err(LlmError::Config { .. })));
    assert!(f.document() == before);
    assert!(f.auth.access_token().await.is_err());
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cleanup_reports_revocation_and_keeps_exact_material_and_cache_rules() {
    for remove in [false, true] {
        for confirmed in [false, true] {
            let f = Fixture::new(3600);
            fs::create_dir(f.store.directory.join("cache")).unwrap();
            let cache = f.store.directory.join("cache/chatgpt-models.json");
            fs::write(&cache, "{}").unwrap();
            f.store.set("deepseek", "unrelated-key-secret").unwrap();
            if !confirmed {
                *f.endpoint.revoke_result.lock().unwrap() = Err(LlmError::Transport {
                    message: "unconfirmed".into(),
                    retryable: true,
                });
            }
            let result = f.auth.clean(remove).await.unwrap();
            assert!(result.removed);
            assert_eq!(
                result.revocation,
                if confirmed {
                    RevocationStatus::Confirmed
                } else {
                    RevocationStatus::Unconfirmed
                }
            );
            assert_eq!(f.endpoint.revoked.lock().unwrap().len(), 1);
            assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 0);
            assert_eq!(cache.exists(), !remove);
            let saved = f.document();
            assert!(saved["providers"]["deepseek"]["key"] == "unrelated-key-secret");
            let entry = &saved["providers"]["chatgpt"];
            assert!(entry.get("tokens").is_none());
            assert_eq!(entry.get("client_id").is_none(), remove);
            assert_eq!(entry.get("account").is_none(), remove);
            let row = f
                .store
                .status()
                .unwrap()
                .providers
                .into_iter()
                .find(|row| row.provider == "chatgpt")
                .unwrap();
            assert_eq!(row.status, AuthState::Missing);
            assert!(row.expires_at.is_none());
            assert_eq!(row.email.is_none(), remove);
            assert_eq!(f.auth.logout().await.unwrap(), RevocationStatus::NotNeeded);
        }
    }
    let (directory, store) = fixture();
    let auth = ChatgptAuth::new(store.clone(), vec![]);
    assert!(!auth.remove().await.unwrap().removed);
    assert!(!directory.path().join("config").exists());
}

#[tokio::test]
async fn concurrent_cleanup_and_cache_failure_preserve_registration_until_cache_is_removed() {
    let f = Fixture::new(3600);
    let cache = f.store.directory.join("cache/chatgpt-models.json");
    fs::create_dir_all(&cache).unwrap();
    assert!(f.auth.remove().await.is_err());
    let saved = f.document();
    assert!(saved["providers"]["chatgpt"].get("tokens").is_none());
    assert!(saved["providers"]["chatgpt"].get("client_id").is_some());
    fs::remove_dir(&cache).unwrap();
    let (logout, remove) = tokio::join!(f.auth.logout(), f.auth.remove());
    assert_eq!(logout.unwrap(), RevocationStatus::NotNeeded);
    assert!(remove.unwrap().removed);
    assert!(
        f.document()["providers"]["chatgpt"]
            .get("client_id")
            .is_none()
    );
    assert!(!f.auth.remove().await.unwrap().removed);
    assert_eq!(f.endpoint.revoked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn refresh_diagnostics_redact_before_bounding_and_status_does_not_refresh() {
    let f = Fixture::new(-1);
    let row = f
        .store
        .status()
        .unwrap()
        .providers
        .into_iter()
        .find(|row| row.provider == "chatgpt")
        .unwrap();
    assert_eq!(row.status, AuthState::Ready);
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 0);
    *f.endpoint.outcome.lock().unwrap() = Outcome::Failure(LlmError::Transport {
        message: format!(
            "refresh failed: {} {} {}",
            "x".repeat(223),
            "access-test-secret",
            "refresh-test-secret"
        ),
        retryable: true,
    });
    f.endpoint.release.add_permits(1);
    let error = f.auth.access_token().await.err().unwrap();
    for secret in ["access-test", "refresh-test", "id-test-secret"] {
        assert!(!format!("{error:?} {error}").contains(secret));
    }
    assert!(f.document()["providers"]["chatgpt"].get("tokens").is_some());
}

#[tokio::test]
async fn cancellation_of_a_logout_waiter_still_finishes_local_cleanup() {
    let f = Fixture::new(3600);
    let gate = Arc::new(Semaphore::new(0));
    *f.endpoint.revoke_gate.lock().unwrap() = Some(gate.clone());
    let auth = f.auth.clone();
    let waiter = tokio::spawn(async move { auth.logout().await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.endpoint.revoke_started.acquire(),
    )
    .await
    .unwrap()
    .unwrap()
    .forget();
    waiter.abort();
    let shutdown = f.auth.shutdown();
    pin_mut!(shutdown);
    assert!(poll!(shutdown.as_mut()).is_pending());
    gate.add_permits(1);
    shutdown.await.unwrap();
    assert!(f.document()["providers"]["chatgpt"].get("tokens").is_none());
    assert_eq!(f.endpoint.revoked.lock().unwrap().len(), 1);
}
