// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncReadExt, net::TcpListener, sync::mpsc};

use super::endpoint::{CodeGrant, ExchangeFailure};
use super::*;
use crate::auth::{AuthState, ProviderAuthStatus};

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
    authorization: SensitiveLoginUrl,
    input: mpsc::Sender<SensitiveLoginUrl>,
    completion: watch::Receiver<Option<Result<ProviderAuthStatus, LoginError>>>,
    control: Arc<LoginControl>,
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

struct Pending {
    host: Secret,
    client: Option<Secret>,
    subject: Option<Secret>,
    hint: Option<Secret>,
    state: Secret,
    nonce: Secret,
    verifier: Secret,
    redirect: String,
}

impl Pending {
    fn new(credentials: &Credentials, redirect: String) -> Result<Self, LlmError> {
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

    fn authorization(&self) -> SensitiveLoginUrl {
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

    fn matches(&self, credentials: &Credentials) -> bool {
        matches!(credentials.0.providers.get("chatgpt"),
            Some(format::Credential::Chatgpt { host_id, client_id, account, .. })
                if host_id == &self.host && client_id.as_ref() == self.client.as_ref()
                    && account.as_ref().map(|account| &account.subject) == self.subject.as_ref())
    }

    fn redaction(&self, credentials: &Credentials) -> Redaction {
        let mut redaction = Redaction::new(credentials);
        for value in [&self.host, &self.state, &self.nonce, &self.verifier] {
            redaction.add(&value.0);
        }
        for value in [&self.client, &self.subject, &self.hint]
            .into_iter()
            .flatten()
        {
            redaction.add(&value.0);
        }
        redaction
    }
}

fn random_secret() -> Result<Secret, LlmError> {
    let mut bytes = zeroize::Zeroizing::new([0; 32]);
    getrandom::fill(&mut *bytes).map_err(|_| failure("system randomness unavailable"))?;
    Ok(Secret(URL_SAFE_NO_PAD.encode(bytes.as_slice())))
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

const MAX_URL_BYTES: usize = 16_384;
const LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(5);

struct Listener {
    listener: TcpListener,
    redirect: String,
    authority: String,
    #[cfg(test)]
    accepted: std::sync::Arc<tokio::sync::Notify>,
}

impl Listener {
    async fn bind(address: SocketAddr) -> Result<Self, LlmError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|error| LlmError::Config {
                message: format!(
                    "ChatGPT callback cannot bind 127.0.0.1:{} ({:?})",
                    address.port(),
                    error.kind()
                ),
            })?;
        let port = listener
            .local_addr()
            .map_err(|_| invalid_callback("cannot inspect listener"))?
            .port();
        let authority = format!("127.0.0.1:{port}");
        Ok(Self {
            listener,
            redirect: format!("http://{authority}/auth/callback"),
            authority,
            #[cfg(test)]
            accepted: Default::default(),
        })
    }

    fn redirect_uri(&self) -> &str {
        &self.redirect
    }

    async fn receive(&self) -> Result<SensitiveLoginUrl, LlmError> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| invalid_callback("listener failed"))?;
            #[cfg(test)]
            self.accepted.notify_one();
            let (sender, mut receiver) = mpsc::channel(1);
            let authority = &self.authority;
            let service = service_fn(|request| {
                let result = response(request, authority, &sender);
                async move { Ok::<_, Infallible>(result) }
            });
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .keep_alive(false)
                .max_headers(32)
                .max_buf_size(MAX_URL_BYTES);
            // hyper 先解析完整头再检查缓冲上限，因此另设包含请求行的读取预算。
            let (read, write) = stream.into_split();
            let stream = tokio::io::join(read.take(MAX_URL_BYTES as u64), write);
            // 取消 receive 会同时释放已经接受但尚未读完请求的连接。
            let _ = tokio::time::timeout(
                LOCAL_IO_TIMEOUT,
                builder.serve_connection(TokioIo::new(stream), service),
            )
            .await;
            if let Ok(url) = receiver.try_recv() {
                return Ok(url);
            }
        }
    }
}

fn response(
    request: Request<Incoming>,
    authority: &str,
    sender: &mpsc::Sender<SensitiveLoginUrl>,
) -> Response<Full<Bytes>> {
    let mut status = StatusCode::OK;
    let mut text = "已收到登录回调，请返回终端查看结果。";
    if request.uri().path() != "/auth/callback" {
        status = StatusCode::NOT_FOUND;
    } else if request.method() != Method::GET {
        status = StatusCode::METHOD_NOT_ALLOWED;
    } else {
        let hosts = request.headers().get_all("host");
        let mut hosts = hosts.iter();
        let valid_host = hosts.next().and_then(|host| host.to_str().ok()) == Some(authority)
            && hosts.next().is_none();
        let valid_uri = request
            .uri()
            .scheme_str()
            .is_none_or(|scheme| scheme == "http")
            && request
                .uri()
                .authority()
                .is_none_or(|value| value.as_str() == authority);
        let has_body = request.headers().contains_key("transfer-encoding")
            || request
                .headers()
                .get("content-length")
                .is_some_and(|value| value != "0");
        let path = request
            .uri()
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("");
        if !valid_host
            || !valid_uri
            || has_body
            || path.len() > MAX_URL_BYTES
            || sender
                .try_send(SensitiveLoginUrl::new(format!("http://{authority}{path}")))
                .is_err()
        {
            status = StatusCode::BAD_REQUEST;
        }
    }
    if status != StatusCode::OK {
        text = "登录回调无效，请返回终端查看结果。";
    }
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Cache-Control", "no-store")
        .header("Connection", "close")
        .body(Full::new(Bytes::from_static(text.as_bytes())))
        .expect("static callback response")
}

struct AuthorizationCode {
    code: Secret,
    client_id: Secret,
}

fn parse_callback(
    input: &SensitiveLoginUrl,
    redirect: &str,
    state: &str,
    saved_client: Option<&str>,
) -> Result<AuthorizationCode, LlmError> {
    if input.expose().len() > MAX_URL_BYTES || input.expose().chars().any(char::is_control) {
        return Err(invalid_callback("invalid callback URL"));
    }
    let url =
        url::Url::parse(input.expose()).map_err(|_| invalid_callback("invalid callback URL"))?;
    let expected = url::Url::parse(redirect).expect("fixed callback URL");
    if input.expose().split('?').next() != Some(redirect)
        || url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid_callback("unexpected callback address"));
    }
    let mut values = BTreeMap::new();
    for pair in url.query().unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key)?;
        let value = Secret(decode(value)?);
        if values.insert(key, value).is_some() {
            return Err(invalid_callback("duplicate callback parameter"));
        }
    }
    if values.get("state").map(|value| value.0.as_str()) != Some(state) {
        return Err(invalid_callback("state mismatch"));
    }
    if values.contains_key("error") {
        return Err(invalid_callback("authorization was declined or failed"));
    }
    let code = values
        .remove("code")
        .filter(|code| !code.0.is_empty())
        .ok_or_else(|| invalid_callback("missing authorization code"))?;
    let returned = values.remove("client_id");
    let client_id = match (saved_client, returned) {
        (Some(saved), None) => Secret(saved.into()),
        (Some(saved), Some(returned)) if returned.0 == saved => returned,
        (Some(_), Some(_)) => return Err(invalid_callback("client ID mismatch")),
        (None, Some(returned)) => returned,
        (None, None) => return Err(invalid_callback("missing issued client ID")),
    };
    if client_id.0.is_empty()
        || client_id.0 == "dynamic_agent_client"
        || client_id.0.chars().any(char::is_control)
    {
        return Err(invalid_callback("invalid issued client ID"));
    }
    Ok(AuthorizationCode { code, client_id })
}

fn decode(value: &str) -> Result<String, LlmError> {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (bytes.get(index + 1).is_none_or(|b| !b.is_ascii_hexdigit())
                || bytes.get(index + 2).is_none_or(|b| !b.is_ascii_hexdigit()))
        {
            return Err(invalid_callback("invalid callback encoding"));
        }
    }
    let value = zeroize::Zeroizing::new(value.replace('+', " "));
    urlencoding::decode(&value)
        .map(|value| value.into_owned())
        .map_err(|_| invalid_callback("invalid callback encoding"))
}

fn invalid_callback(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT callback: {reason}"),
    }
}

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
            if state.stopped || state.logins_stopped || state.cleaning != 0 || state.login.is_some()
            {
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
        let mut redaction = pending.redaction(&credentials);
        redaction.add(callback.expose());
        let code = parse_callback(
            &callback,
            &pending.redirect,
            &pending.state.0,
            pending.client.as_ref().map(|client| client.0.as_str()),
        )
        .map_err(|error| redaction.apply(error))?;
        redaction.add(&code.code.0);
        redaction.add(&code.client_id.0);
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
        let unverified = match exchange {
            Ok(material) => material,
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
            Err(ExchangeFailure::Other(error)) => return Err(redaction.apply(error).into()),
        };
        unverified.add_secrets(&mut redaction);
        let (identity, tokens) = tokio::select! {
            biased;
            _ = control.cancelled() => return Err(LoginError::Cancelled),
            result = self.endpoint.verify(unverified, &code.client_id.0, &pending.nonce.0) => {
                result.map_err(|error| redaction.apply(error))?
            },
        };
        if pending
            .subject
            .as_ref()
            .is_some_and(|subject| subject != &identity.subject)
        {
            return Err(failure("account mismatch or missing direct permission").into());
        }
        let status = ProviderAuthStatus {
            provider: "chatgpt".into(),
            status: AuthState::Ready,
            email: Some(identity.email.clone()),
            expires_at: Some(tokens.expires_at.clone()),
        };
        self.save_login(
            control,
            &pending,
            format::Credential::Chatgpt {
                host_id: Secret(pending.host.0.clone()),
                client_id: Some(code.client_id),
                account: Some(identity),
                tokens: Some(tokens),
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
            || state.logins_stopped
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
mod callback_tests;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod tests;
