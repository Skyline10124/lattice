// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{collections::BTreeMap, convert::Infallible, net::SocketAddr, time::Duration};

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::{io::AsyncReadExt, net::TcpListener, sync::mpsc};

use super::{Secret, login::SensitiveLoginUrl};
use crate::LlmError;

pub(super) const MAX_URL_BYTES: usize = 16_384;
const LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Listener {
    listener: TcpListener,
    redirect: String,
    authority: String,
    #[cfg(test)]
    accepted: std::sync::Arc<tokio::sync::Notify>,
}

impl Listener {
    pub(super) async fn bind(address: SocketAddr) -> Result<Self, LlmError> {
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
            .map_err(|_| invalid("cannot inspect listener"))?
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

    pub(super) fn redirect_uri(&self) -> &str {
        &self.redirect
    }

    pub(super) async fn receive(&self) -> Result<SensitiveLoginUrl, LlmError> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| invalid("listener failed"))?;
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

pub(super) struct AuthorizationCode {
    pub code: Secret,
    pub client_id: Secret,
}

pub(super) fn parse(
    input: &SensitiveLoginUrl,
    redirect: &str,
    state: &str,
    saved_client: Option<&str>,
) -> Result<AuthorizationCode, LlmError> {
    if input.expose().len() > MAX_URL_BYTES || input.expose().chars().any(char::is_control) {
        return Err(invalid("invalid callback URL"));
    }
    let url = url::Url::parse(input.expose()).map_err(|_| invalid("invalid callback URL"))?;
    let expected = url::Url::parse(redirect).expect("fixed callback URL");
    if input.expose().split('?').next() != Some(redirect)
        || url.origin() != expected.origin()
        || url.path() != expected.path()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid("unexpected callback address"));
    }
    let mut values = BTreeMap::new();
    for pair in url.query().unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key)?;
        let value = Secret(decode(value)?);
        if values.insert(key, value).is_some() {
            return Err(invalid("duplicate callback parameter"));
        }
    }
    if values.get("state").map(|value| value.0.as_str()) != Some(state) {
        return Err(invalid("state mismatch"));
    }
    if values.contains_key("error") {
        return Err(invalid("authorization was declined or failed"));
    }
    let code = values
        .remove("code")
        .filter(|code| !code.0.is_empty())
        .ok_or_else(|| invalid("missing authorization code"))?;
    let returned = values.remove("client_id");
    let client_id = match (saved_client, returned) {
        (Some(saved), None) => Secret(saved.into()),
        (Some(saved), Some(returned)) if returned.0 == saved => returned,
        (Some(_), Some(_)) => return Err(invalid("client ID mismatch")),
        (None, Some(returned)) => returned,
        (None, None) => return Err(invalid("missing issued client ID")),
    };
    if client_id.0.is_empty()
        || client_id.0 == "dynamic_agent_client"
        || client_id.0.chars().any(char::is_control)
    {
        return Err(invalid("invalid issued client ID"));
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
            return Err(invalid("invalid callback encoding"));
        }
    }
    let value = zeroize::Zeroizing::new(value.replace('+', " "));
    urlencoding::decode(&value)
        .map(|value| value.into_owned())
        .map_err(|_| invalid("invalid callback encoding"))
}

fn invalid(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT callback: {reason}"),
    }
}

#[cfg(test)]
mod tests;
