// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::auth::{
    id_token::tests::{claims, keys, signed},
    oauth::{LoginTokens, TokenReply},
    tests::{fixture, write},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::future::BoxFuture;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Exchange,
    Discovery,
    Jwks,
    Refresh,
}

struct Endpoint {
    claims: Mutex<Value>,
    invalid_grant: Mutex<bool>,
    error: Mutex<Option<LlmError>>,
    scopes: Mutex<Vec<String>>,
    calls: AtomicUsize,
    grants: Mutex<Vec<(Secret, String)>>,
    pause: Mutex<Option<(Stage, Arc<Semaphore>)>>,
    started: Semaphore,
    revoked: Mutex<Vec<String>>,
}

impl Endpoint {
    fn new() -> Self {
        Self {
            claims: Mutex::new(claims("client", "unset")),
            invalid_grant: Mutex::new(false),
            error: Mutex::new(None),
            scopes: Mutex::new(vec![DIRECT_SCOPE.into()]),
            calls: AtomicUsize::new(0),
            grants: Mutex::new(vec![]),
            pause: Mutex::new(None),
            started: Semaphore::new(0),
            revoked: Mutex::new(vec![]),
        }
    }

    fn pause(&self, stage: Stage) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        *self.pause.lock().unwrap() = Some((stage, gate.clone()));
        gate
    }

    async fn stage(&self, stage: Stage) {
        let gate = self
            .pause
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(selected, _)| *selected == stage)
            .map(|(_, gate)| gate.clone());
        if let Some(gate) = gate {
            self.started.add_permits(1);
            tokio::time::timeout(Duration::from_secs(5), gate.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
        }
    }

    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.started.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }

    fn token(&self, refreshed: bool) -> TokenReply {
        TokenReply {
            access_token: Secret(
                if refreshed {
                    "rotated-access-secret"
                } else {
                    "new-access-secret"
                }
                .into(),
            ),
            refresh_token: Secret(
                if refreshed {
                    "rotated-refresh-secret"
                } else {
                    "new-refresh-secret"
                }
                .into(),
            ),
            expires_at: (Utc::now() + TimeDelta::hours(1)).to_rfc3339(),
            scopes: self.scopes.lock().unwrap().clone(),
        }
    }
}

impl TokenEndpoint for Endpoint {
    fn exchange<'a>(
        &'a self,
        grant: CodeGrant<'a>,
    ) -> BoxFuture<'a, Result<LoginTokens, ExchangeFailure>> {
        Box::pin(async move {
            assert_eq!(grant.client_id, "client");
            assert_eq!(grant.code, "local-code");
            self.grants
                .lock()
                .unwrap()
                .push((Secret(grant.verifier.into()), grant.redirect_uri.into()));
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.stage(Stage::Exchange).await;
            if *self.invalid_grant.lock().unwrap() {
                return Err(ExchangeFailure::InvalidGrant);
            }
            if let Some(error) = self.error.lock().unwrap().clone() {
                return Err(ExchangeFailure::Other(error));
            }
            Ok(LoginTokens {
                token: self.token(false),
                id_token: Secret(signed(&self.claims.lock().unwrap())),
            })
        })
    }
    fn discovery(&self) -> BoxFuture<'_, Result<url::Url, LlmError>> {
        Box::pin(async {
            self.stage(Stage::Discovery).await;
            Ok(url::Url::parse("https://auth.openai.com/local-test-keys").unwrap())
        })
    }
    fn jwks<'a>(
        &'a self,
        _: &'a url::Url,
    ) -> BoxFuture<'a, Result<jsonwebtoken::jwk::JwkSet, LlmError>> {
        Box::pin(async {
            self.stage(Stage::Jwks).await;
            Ok(keys())
        })
    }
    fn refresh<'a>(
        &'a self,
        _: RefreshGrant<'a>,
    ) -> BoxFuture<'a, Result<TokenReply, RefreshFailure>> {
        Box::pin(async {
            self.stage(Stage::Refresh).await;
            Ok(self.token(true))
        })
    }
    fn revoke<'a>(&'a self, grant: RefreshGrant<'a>) -> BoxFuture<'a, Result<(), LlmError>> {
        Box::pin(async move {
            self.revoked
                .lock()
                .unwrap()
                .push(grant.refresh_token.into());
            Ok(())
        })
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    store: AuthStore,
    auth: ChatgptAuth,
    endpoint: Arc<Endpoint>,
}

impl Fixture {
    fn new() -> Self {
        let (directory, store) = fixture();
        let endpoint = Arc::new(Endpoint::new());
        let auth =
            ChatgptAuth::with_dependencies(store.clone(), endpoint.clone(), Arc::new(Utc::now));
        Self {
            _directory: directory,
            store,
            auth,
            endpoint,
        }
    }

    async fn start(&self) -> ChatgptLogin {
        self.auth
            .start_login_at("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap()
    }

    fn callback(&self, login: &ChatgptLogin, issued_id: bool) -> SensitiveLoginUrl {
        let parsed = url::Url::parse(login.authorization_url().expose()).unwrap();
        let values: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        self.endpoint.claims.lock().unwrap()["nonce"] = json!(values["nonce"]);
        let mut callback = url::Url::parse(&values["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", &values["state"])
            .append_pair("code", "local-code");
        if issued_id {
            callback
                .query_pairs_mut()
                .append_pair("client_id", "client");
        }
        SensitiveLoginUrl::new(callback.into())
    }

    fn document(&self) -> Value {
        serde_json::from_slice(&fs::read(self.store.directory.join("auth.json")).unwrap()).unwrap()
    }

    async fn finish(&self, login: &ChatgptLogin) -> Result<ProviderAuthStatus, LoginError> {
        tokio::time::timeout(Duration::from_secs(5), login.wait())
            .await
            .unwrap()
    }
}

fn address(login: &ChatgptLogin) -> SocketAddr {
    let url = url::Url::parse(login.authorization_url().expose()).unwrap();
    let redirect: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    url::Url::parse(&redirect["redirect_uri"])
        .unwrap()
        .socket_addrs(|| None)
        .unwrap()[0]
}

async fn drop_and_wait(login: ChatgptLogin) -> Result<ProviderAuthStatus, LoginError> {
    let mut completion = login.completion.clone();
    drop(login);
    tokio::time::timeout(
        Duration::from_secs(2),
        completion.wait_for(|result| result.is_some()),
    )
    .await
    .expect("Drop 后登录应在 manager 仍存活时自行结束")
    .unwrap()
    .clone()
    .unwrap()
}

#[tokio::test]
async fn login_commits_version_one_identity_and_preserves_parallel_api_key_write() {
    let f = Fixture::new();
    let login = f.start().await;
    let authorization = url::Url::parse(login.authorization_url().expose()).unwrap();
    let parameters: std::collections::BTreeMap<_, _> =
        authorization.query_pairs().into_owned().collect();
    let callback = f.callback(&login, true);
    f.store.set("openai", "parallel-api-key").unwrap();
    let browser = reqwest::Client::builder().no_proxy().build().unwrap();
    let receipt = browser
        .get(callback.expose())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(receipt.contains("已收到登录回调"));
    assert!(!receipt.contains("local-code"));
    let result = f.finish(&login).await.unwrap();
    assert_eq!(result.status, AuthState::Ready);
    let public = serde_json::to_string(&result).unwrap();
    for secret in [
        "local-subject",
        "client_id",
        "new-access-secret",
        "new-refresh-secret",
    ] {
        assert!(!public.contains(secret));
    }
    let document = f.document();
    assert_eq!(document["version"], 1);
    assert_eq!(
        document["providers"]["chatgpt"]["account"]["subject"],
        "local-subject"
    );
    assert_eq!(
        document["providers"]["chatgpt"]["tokens"]["refresh_token"],
        "new-refresh-secret"
    );
    assert_eq!(
        f.store.load().unwrap().api_key("openai"),
        Some("parallel-api-key")
    );
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
    {
        let grants = f.endpoint.grants.lock().unwrap();
        let (verifier, redirect) = &grants[0];
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.0.as_bytes())),
            parameters["code_challenge"]
        );
        assert_eq!(redirect, &parameters["redirect_uri"]);
    }
    assert!(tokio::net::TcpListener::bind(address(&login)).await.is_ok());
}

#[tokio::test]
async fn invalid_grant_retains_issued_registration_for_next_login_only() {
    let f = Fixture::new();
    *f.endpoint.invalid_grant.lock().unwrap() = true;
    let login = f.start().await;
    let first_host = f.document()["providers"]["chatgpt"]["host_id"].clone();
    login
        .submit_callback(f.callback(&login, true))
        .await
        .unwrap();
    assert!(f.finish(&login).await.is_err());
    let retained = f.document()["providers"]["chatgpt"].clone();
    assert_eq!(retained["client_id"], "client");
    assert!(retained.get("tokens").is_none() && retained.get("account").is_none());
    let returning = f.start().await;
    assert!(
        !returning
            .authorization_url()
            .expose()
            .contains("dynamic_agent_client")
    );
    *f.endpoint.invalid_grant.lock().unwrap() = false;
    returning
        .submit_callback(f.callback(&returning, false))
        .await
        .unwrap();
    f.finish(&returning).await.unwrap();
    assert_eq!(f.document()["providers"]["chatgpt"]["host_id"], first_host);
    let before = f.document();
    *f.endpoint.invalid_grant.lock().unwrap() = true;
    let third = f.start().await;
    third
        .submit_callback(f.callback(&third, false))
        .await
        .unwrap();
    assert!(f.finish(&third).await.is_err());
    assert_eq!(f.document(), before);
}

#[tokio::test]
async fn rejected_callbacks_do_not_exchange_or_save_registration() {
    for query in [
        "state=wrong&code=local-code&client_id=client",
        "code=local-code&client_id=client",
        "error=access_denied",
    ] {
        let f = Fixture::new();
        let login = f.start().await;
        let url = f.callback(&login, true);
        let mut parsed = url::Url::parse(url.expose()).unwrap();
        parsed.set_query(Some(query));
        login
            .submit_callback(SensitiveLoginUrl::new(parsed.into()))
            .await
            .unwrap();
        assert!(f.finish(&login).await.is_err());
        assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 0);
        assert!(
            f.document()["providers"]["chatgpt"]
                .get("client_id")
                .is_none()
        );
    }
}

#[tokio::test]
async fn nonce_scope_and_subject_failure_preserve_existing_login() {
    let f = Fixture::new();
    let first = f.start().await;
    first
        .submit_callback(f.callback(&first, true))
        .await
        .unwrap();
    f.finish(&first).await.unwrap();
    let before = f.document();
    for failure_case in 0..3 {
        *f.endpoint.claims.lock().unwrap() = claims("client", "unset");
        *f.endpoint.scopes.lock().unwrap() = vec![DIRECT_SCOPE.into()];
        let login = f.start().await;
        let callback = f.callback(&login, false);
        match failure_case {
            0 => f.endpoint.claims.lock().unwrap()["nonce"] = json!("wrong-nonce"),
            1 => f.endpoint.claims.lock().unwrap()["sub"] = json!("other-subject"),
            _ => *f.endpoint.scopes.lock().unwrap() = vec!["openid".into()],
        }
        login.submit_callback(callback).await.unwrap();
        assert!(f.finish(&login).await.is_err());
        assert_eq!(f.document(), before);
    }
    *f.endpoint.claims.lock().unwrap() = claims("client", "unset");
    f.endpoint.claims.lock().unwrap()["email"] = json!("updated@example.test");
    *f.endpoint.scopes.lock().unwrap() = vec![DIRECT_SCOPE.into()];
    let login = f.start().await;
    login
        .submit_callback(f.callback(&login, false))
        .await
        .unwrap();
    assert_eq!(
        f.finish(&login).await.unwrap().email.as_deref(),
        Some("updated@example.test")
    );
}

#[tokio::test]
async fn oversized_paste_fails_the_attempt_and_both_callback_sources_exchange_once() {
    let f = Fixture::new();
    let login = f.start().await;
    let port = address(&login);
    login
        .submit_callback(SensitiveLoginUrl::new(
            "x".repeat(callback::MAX_URL_BYTES + 1),
        ))
        .await
        .unwrap();
    assert!(f.finish(&login).await.is_err());
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 0);
    assert!(tokio::net::TcpListener::bind(port).await.is_ok());
    let login = f.start().await;
    let callback = f.callback(&login, true);
    let browser = reqwest::Client::builder().no_proxy().build().unwrap();
    let (_browser, _paste) = tokio::join!(
        browser.get(callback.expose()).send(),
        login.submit_callback(SensitiveLoginUrl::new(callback.expose().into())),
    );
    f.finish(&login).await.unwrap();
    assert_eq!(f.endpoint.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancellation_in_each_network_stage_and_shutdown_release_resources() {
    for stage in [Stage::Exchange, Stage::Discovery, Stage::Jwks] {
        for drop_handle in [false, true] {
            let f = Fixture::new();
            let _gate = f.endpoint.pause(stage);
            let login = f.start().await;
            let port = address(&login);
            let before = f.document();
            login
                .submit_callback(f.callback(&login, true))
                .await
                .unwrap();
            f.endpoint.started().await;
            let outcome = if drop_handle {
                drop_and_wait(login).await
            } else {
                login.cancel();
                f.finish(&login).await
            };
            assert!(matches!(outcome, Err(LoginError::Cancelled)));
            assert_eq!(f.document(), before);
            assert!(tokio::net::TcpListener::bind(port).await.is_ok());
            f.auth.shutdown().await.unwrap();
        }
    }
    let f = Fixture::new();
    let login = f.start().await;
    let port = address(&login);
    f.auth.shutdown().await.unwrap();
    assert!(matches!(f.finish(&login).await, Err(LoginError::Cancelled)));
    assert!(tokio::net::TcpListener::bind(port).await.is_ok());
    assert!(f.auth.start_login().await.is_err());
}

#[tokio::test]
async fn dropping_login_or_startup_future_does_not_leave_background_listener() {
    let f = Fixture::new();
    let login = f.start().await;
    let port = address(&login);
    assert!(matches!(
        drop_and_wait(login).await,
        Err(LoginError::Cancelled)
    ));
    assert!(tokio::net::TcpListener::bind(port).await.is_ok());
    f.auth.shutdown().await.unwrap();
    let f = Fixture::new();
    let operation = f.auth.0.operation.lock().await;
    let auth = f.auth.clone();
    let start =
        tokio::spawn(async move { auth.start_login_at("127.0.0.1:0".parse().unwrap()).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.auth.0.state.lock().unwrap().login.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    start.abort();
    assert!(start.await.err().unwrap().is_cancelled());
    drop(operation);
    f.auth.shutdown().await.unwrap();
}

async fn cancel_queued_write(invalid_grant: bool) {
    let f = Fixture::new();
    if !invalid_grant {
        let first = f.start().await;
        first
            .submit_callback(f.callback(&first, true))
            .await
            .unwrap();
        f.finish(&first).await.unwrap();
        f.endpoint.claims.lock().unwrap()["email"] = json!("replacement@example.test");
    }
    *f.endpoint.invalid_grant.lock().unwrap() = invalid_grant;
    let login = f.start().await;
    let before = f.document();
    let (entered, queued) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    *f.auth.0.before_login_write.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        gate.recv_timeout(Duration::from_secs(5)).unwrap();
    }));
    login
        .submit_callback(f.callback(&login, invalid_grant))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), queued)
        .await
        .unwrap()
        .unwrap();
    login.cancel();
    release.send(()).unwrap();
    let result = f.finish(&login).await;
    assert!(matches!(result, Err(LoginError::Cancelled)));
    assert_eq!(f.document(), before);
    f.auth.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_after_login_write_is_queued_preserves_original_credentials() {
    cancel_queued_write(false).await;
}

#[tokio::test]
async fn cancellation_after_registration_write_is_queued_preserves_host_only_record() {
    cancel_queued_write(true).await;
}

#[tokio::test]
async fn logout_remove_and_replaced_registration_reject_late_login() {
    for remove in [true, false] {
        let f = Fixture::new();
        let gate = f.endpoint.pause(Stage::Exchange);
        let login = f.start().await;
        login
            .submit_callback(f.callback(&login, true))
            .await
            .unwrap();
        f.endpoint.started().await;
        if remove {
            f.auth.remove().await.unwrap();
        } else {
            f.auth.logout().await.unwrap();
        }
        gate.add_permits(1);
        assert!(matches!(f.finish(&login).await, Err(LoginError::Cancelled)));
        assert!(f.document()["providers"]["chatgpt"].get("tokens").is_none());
    }
    let f = Fixture::new();
    let gate = f.endpoint.pause(Stage::Exchange);
    let login = f.start().await;
    login
        .submit_callback(f.callback(&login, true))
        .await
        .unwrap();
    f.endpoint.started().await;
    let mut replacement = f.document();
    replacement["providers"]["chatgpt"]["client_id"] = json!("replacement-registration");
    write(&f.store, serde_json::to_vec(&replacement).unwrap());
    gate.add_permits(1);
    assert!(f.finish(&login).await.is_err());
    assert_eq!(f.document(), replacement);
}

#[tokio::test]
async fn refresh_during_browser_login_can_commit_before_new_login_and_logout_uses_latest_token() {
    let f = Fixture::new();
    let first = f.start().await;
    first
        .submit_callback(f.callback(&first, true))
        .await
        .unwrap();
    f.finish(&first).await.unwrap();
    let mut document = f.document();
    document["providers"]["chatgpt"]["tokens"]["expires_at"] =
        json!((Utc::now() - TimeDelta::seconds(1)).to_rfc3339());
    write(&f.store, serde_json::to_vec(&document).unwrap());
    let gate = f.endpoint.pause(Stage::Exchange);
    let returning = f.start().await;
    returning
        .submit_callback(f.callback(&returning, false))
        .await
        .unwrap();
    f.endpoint.started().await;
    let token = f.auth.access_token().await.unwrap();
    assert_eq!(
        token.authorization().unwrap(),
        "Bearer rotated-access-secret"
    );
    gate.add_permits(1);
    f.finish(&returning).await.unwrap();
    assert_eq!(
        f.document()["providers"]["chatgpt"]["tokens"]["access_token"],
        "new-access-secret"
    );
    f.auth.logout().await.unwrap();
    assert_eq!(
        &*f.endpoint.revoked.lock().unwrap(),
        &["new-refresh-secret"]
    );
}

#[tokio::test]
async fn transport_failure_redacts_pending_and_disk_values_together() {
    let f = Fixture::new();
    let login = f.start().await;
    let callback = f.callback(&login, true);
    let parsed = url::Url::parse(login.authorization_url().expose()).unwrap();
    let values: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
    f.store.set("openai", "local").unwrap();
    *f.endpoint.error.lock().unwrap() = Some(LlmError::ProviderHttp {
        status_code: 500,
        retryable: true,
        retry_after: None,
        message: format!(
            "{} local-code client {}",
            values["state"],
            callback.expose()
        ),
    });
    login.submit_callback(callback).await.unwrap();
    let error = f.finish(&login).await.err().unwrap();
    let output = format!("{error:?} {error}");
    assert!(!output.contains("local-code"));
    assert!(!output.contains(&values["state"]));
    assert!(!output.contains("http://127."));
    assert!(
        f.document()["providers"]["chatgpt"]
            .get("client_id")
            .is_none()
    );
}
