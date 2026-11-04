// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{Secret, valid_access_token};
use crate::{LlmError, transport};

const ISSUER: &str = "https://auth.openai.com";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub(super) const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
pub(super) type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub(super) struct RefreshGrant<'a> {
    pub client_id: &'a str,
    pub refresh_token: &'a str,
}

pub(super) struct TokenReply {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub expires_at: String,
    pub scopes: Vec<String>,
}

pub(super) enum RefreshFailure {
    InvalidToken(&'static str),
    Other(LlmError),
}

pub(super) trait TokenEndpoint: Send + Sync {
    fn refresh<'a>(
        &'a self,
        grant: RefreshGrant<'a>,
    ) -> BoxFuture<'a, Result<TokenReply, RefreshFailure>>;

    fn revoke<'a>(&'a self, grant: RefreshGrant<'a>) -> BoxFuture<'a, Result<(), LlmError>>;
}

pub(super) struct HttpTokenEndpoint {
    pub roots: Vec<reqwest::Certificate>,
    pub clock: Clock,
}

struct HttpReply {
    status: u16,
    headers: HeaderMap,
    body: Zeroizing<Vec<u8>>,
}

impl HttpTokenEndpoint {
    async fn request(
        &self,
        url: &str,
        form: Option<Zeroizing<Vec<u8>>>,
    ) -> Result<HttpReply, LlmError> {
        tokio::time::timeout(TIMEOUT, async {
            let roots = self.roots.clone();
            let address = url.to_owned();
            let client = tokio::task::spawn_blocking(move || {
                if roots.is_empty() {
                    return Err(configuration("no usable TLS root certificates"));
                }
                let builder = reqwest::Client::builder()
                    .https_only(true)
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .tls_certs_only(roots)
                    .timeout(TIMEOUT)
                    .connect_timeout(TIMEOUT)
                    .read_timeout(TIMEOUT)
                    .user_agent(transport::user_agent(None));
                crate::client::pin_http_client(&address, builder)
                    .map_err(|error| match error {
                        net_guard::NetGuardError::Config { .. } => {
                            configuration("OAuth network policy rejected the endpoint")
                        }
                        net_guard::NetGuardError::Dns { .. } => LlmError::Transport {
                            message: "ChatGPT OAuth DNS resolution failed".into(),
                            retryable: true,
                        },
                    })?
                    .build()
                    .map_err(|_| configuration("cannot build OAuth HTTP client"))
            })
            .await
            .map_err(|_| configuration("OAuth client worker failed"))??;
            let request = match form {
                Some(form) => client
                    .post(url)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(bytes::Bytes::from_owner(form)),
                None => client.get(url),
            };
            let response = request
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(network_error)?;
            read_reply(response).await
        })
        .await
        .map_err(|_| timeout_error())?
    }
}

async fn read_reply(mut response: reqwest::Response) -> Result<HttpReply, LlmError> {
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(invalid_response("response exceeds 1 MiB"));
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        if error.is_timeout() {
            timeout_error()
        } else {
            LlmError::Transport {
                message: "ChatGPT OAuth response body could not be read".into(),
                retryable: true,
            }
        }
    })? {
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            return Err(invalid_response("response exceeds 1 MiB"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(HttpReply {
        status,
        headers,
        body,
    })
}

impl TokenEndpoint for HttpTokenEndpoint {
    fn refresh<'a>(
        &'a self,
        grant: RefreshGrant<'a>,
    ) -> BoxFuture<'a, Result<TokenReply, RefreshFailure>> {
        Box::pin(async move {
            let response = self
                .request(TOKEN_URL, Some(refresh_form(&grant)))
                .await
                .map_err(RefreshFailure::Other)?;
            if response.status != 200 {
                return Err(refresh_error(&response, (self.clock)()));
            }
            parse_token(&response.body, (self.clock)()).map_err(RefreshFailure::Other)
        })
    }

    fn revoke<'a>(&'a self, grant: RefreshGrant<'a>) -> BoxFuture<'a, Result<(), LlmError>> {
        Box::pin(async move {
            // 撤销的发现与 POST 共用一个总时限，任一步失败都不重试。
            tokio::time::timeout(TIMEOUT, async {
                let discovery = self.request(DISCOVERY_URL, None).await?;
                if discovery.status != 200 {
                    return Err(invalid_response("revocation discovery failed"));
                }
                let endpoint = revocation_endpoint(&discovery.body)?;
                let reply = self
                    .request(endpoint.as_str(), Some(revocation_form(&grant)))
                    .await?;
                confirm_revocation(reply.status, &reply.body)
            })
            .await
            .map_err(|_| timeout_error())?
        })
    }
}

fn encoded_form(pairs: &[(&str, &str)]) -> Zeroizing<Vec<u8>> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.extend_pairs(pairs.iter().copied());
    Zeroizing::new(form.finish().into_bytes())
}

fn refresh_form(grant: &RefreshGrant<'_>) -> Zeroizing<Vec<u8>> {
    encoded_form(&[
        ("grant_type", "refresh_token"),
        ("client_id", grant.client_id),
        ("refresh_token", grant.refresh_token),
        ("resource", "https://api.openai.com/v1"),
    ])
}

fn revocation_form(grant: &RefreshGrant<'_>) -> Zeroizing<Vec<u8>> {
    encoded_form(&[
        ("token", grant.refresh_token),
        ("token_type_hint", "refresh_token"),
        ("client_id", grant.client_id),
    ])
}

fn revocation_endpoint(body: &[u8]) -> Result<url::Url, LlmError> {
    #[derive(Deserialize)]
    struct Discovery {
        issuer: String,
        revocation_endpoint: String,
    }
    let discovery: Discovery = serde_json::from_slice(body)
        .map_err(|_| invalid_response("invalid revocation discovery"))?;
    let url = url::Url::parse(&discovery.revocation_endpoint)
        .map_err(|_| configuration("invalid revocation endpoint"))?;
    if discovery.issuer != ISSUER
        || url.origin().ascii_serialization() != ISSUER
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(configuration("untrusted revocation endpoint"));
    }
    Ok(url)
}

fn confirm_revocation(status: u16, body: &[u8]) -> Result<(), LlmError> {
    if status == 200 && body.is_empty() {
        Ok(())
    } else {
        Err(invalid_response("remote revocation was not confirmed"))
    }
}

fn parse_token(body: &[u8], now: DateTime<Utc>) -> Result<TokenReply, LlmError> {
    // 刷新响应中的 id_token 无论取值为何都不读取，保留登录时验证并保存的那份。
    #[derive(Deserialize)]
    struct Reply {
        access_token: Secret,
        refresh_token: Secret,
        token_type: String,
        expires_in: u64,
        scope: String,
    }
    let reply: Reply =
        serde_json::from_slice(body).map_err(|_| invalid_response("invalid token response"))?;
    let seconds = i64::try_from(reply.expires_in)
        .ok()
        .and_then(chrono::TimeDelta::try_seconds)
        .filter(|duration| *duration > chrono::TimeDelta::zero());
    let expires_at = seconds
        .and_then(|duration| now.checked_add_signed(duration))
        .ok_or_else(|| invalid_response("invalid token expiry"))?;
    let scopes: Vec<String> = reply.scope.split_whitespace().map(str::to_owned).collect();
    if !reply.token_type.eq_ignore_ascii_case("bearer")
        || !valid_access_token(&reply.access_token.0)
        || reply.refresh_token.0.is_empty()
        || reply.refresh_token.0.chars().any(char::is_control)
        || !scopes.iter().any(|scope| scope == DIRECT_SCOPE)
    {
        return Err(invalid_response(
            "invalid token material or missing direct permission",
        ));
    }
    Ok(TokenReply {
        access_token: reply.access_token,
        refresh_token: reply.refresh_token,
        expires_at: expires_at.to_rfc3339(),
        scopes,
    })
}

fn refresh_error(reply: &HttpReply, now: DateTime<Utc>) -> RefreshFailure {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Code {
        Plain(Secret),
        Object { code: Secret },
    }
    #[derive(Deserialize)]
    struct ErrorReply {
        error: Code,
    }
    let parsed: Option<ErrorReply> = serde_json::from_slice(&reply.body).ok();
    let code = parsed.as_ref().map(|reply| match &reply.error {
        Code::Plain(code) | Code::Object { code } => code.0.as_str(),
    });
    let terminal = match code {
        Some("invalid_grant") => Some("invalid_grant"),
        Some("invalid_refresh_token") => Some("invalid_refresh_token"),
        Some("token_expired") => Some("token_expired"),
        Some("refresh_token_expired") => Some("refresh_token_expired"),
        Some("refresh_token_invalidated") => Some("refresh_token_invalidated"),
        Some("refresh_token_reused") => Some("refresh_token_reused"),
        _ => None,
    };
    if let Some(code) = terminal {
        return RefreshFailure::InvalidToken(code);
    }
    let invalid_client = code == Some("invalid_client");
    let mut message = format!("ChatGPT token refresh failed: HTTP {}", reply.status);
    if invalid_client {
        message.push_str(
            " invalid_client; run lattice auth remove chatgpt, then lattice auth login chatgpt",
        );
    }
    if let Some(id) = transport::provider_request_id(&reply.headers) {
        message.push_str("; request_id=");
        message.push_str(&id);
    }
    RefreshFailure::Other(LlmError::ProviderHttp {
        status_code: reply.status,
        message,
        retryable: !invalid_client
            && (reply.status >= 500 || matches!(reply.status, 408 | 409 | 429)),
        retry_after: transport::retry_after(&reply.headers, now.into()),
    })
}

fn configuration(message: &str) -> LlmError {
    LlmError::Config {
        message: format!("ChatGPT OAuth: {message}"),
    }
}

fn invalid_response(message: &str) -> LlmError {
    LlmError::Other(format!(
        "ChatGPT token refresh/revocation failed: {message}"
    ))
}

fn timeout_error() -> LlmError {
    LlmError::Timeout {
        message: "ChatGPT token refresh/revocation did not finish within 30 seconds".into(),
    }
}

fn network_error(error: reqwest::Error) -> LlmError {
    if error.is_timeout() {
        timeout_error()
    } else {
        LlmError::Transport {
            message: "ChatGPT token refresh/revocation network request failed".into(),
            retryable: error.is_connect() || error.is_request() || error.is_body(),
        }
    }
}

#[cfg(test)]
mod tests;
