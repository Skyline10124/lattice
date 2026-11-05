// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::{future::Future, time::Duration};

use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use reqwest::header::HeaderMap;
use serde::Deserialize;
use zeroize::Zeroizing;

use super::{Clock, DIRECT_SCOPE, Redaction, Secret, format, valid_access_token};
use crate::{LlmError, transport};
use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    jwk::{AlgorithmParameters, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse},
};

const ISSUER: &str = "https://auth.openai.com";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const DISCOVERY_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 1_048_576;

struct RefreshGrant<'a> {
    client_id: &'a str,
    refresh_token: &'a str,
}

struct TokenReply {
    access_token: Secret,
    refresh_token: Secret,
    expires_at: String,
    scopes: Vec<String>,
}

pub(super) struct CodeGrant<'a> {
    pub(super) client_id: &'a str,
    pub(super) code: &'a str,
    pub(super) verifier: &'a str,
    pub(super) redirect_uri: &'a str,
}

/// 字段保持私有，使未验签 token 只能交给 verify 或加入脱敏集合，不能直接保存。
pub(super) struct UnverifiedLogin {
    tokens: format::Tokens,
}

impl UnverifiedLogin {
    pub(super) fn add_secrets(&self, redaction: &mut Redaction) {
        redaction.add(&self.tokens.access_token.0);
        redaction.add(&self.tokens.refresh_token.0);
        redaction.add(&self.tokens.id_token.0);
    }
}

impl TokenReply {
    fn with_identity(self, id_token: Secret) -> format::Tokens {
        format::Tokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            id_token,
            expires_at: self.expires_at,
            scopes: self.scopes,
        }
    }
}

pub(super) enum ExchangeFailure {
    InvalidGrant,
    Other(LlmError),
}

pub(super) enum RefreshFailure {
    InvalidToken(&'static str),
    Other(LlmError),
}

pub(super) trait TokenEndpoint: Send + Sync {
    fn exchange<'a>(
        &'a self,
        grant: CodeGrant<'a>,
    ) -> BoxFuture<'a, Result<UnverifiedLogin, ExchangeFailure>>;

    fn verify<'a>(
        &'a self,
        login: UnverifiedLogin,
        client_id: &'a str,
        nonce: &'a str,
    ) -> BoxFuture<'a, Result<(format::Account, format::Tokens), LlmError>>;

    fn refresh<'a>(
        &'a self,
        client_id: &'a str,
        previous: &'a format::Tokens,
    ) -> BoxFuture<'a, Result<format::Tokens, RefreshFailure>>;

    fn revoke<'a>(
        &'a self,
        client_id: &'a str,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<(), LlmError>>;
}

pub(super) struct HttpTokenEndpoint {
    roots: Vec<reqwest::Certificate>,
    clock: Clock,
}

struct HttpReply {
    status: u16,
    headers: HeaderMap,
    body: Zeroizing<Vec<u8>>,
}

impl HttpTokenEndpoint {
    pub(super) fn new(roots: Vec<reqwest::Certificate>, clock: Clock) -> Self {
        Self { roots, clock }
    }

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
    fn exchange<'a>(
        &'a self,
        grant: CodeGrant<'a>,
    ) -> BoxFuture<'a, Result<UnverifiedLogin, ExchangeFailure>> {
        Box::pin(async move {
            let response = self
                .request(TOKEN_URL, Some(code_form(&grant)))
                .await
                .map_err(|error| ExchangeFailure::Other(phase_error(error, "code exchange")))?;
            exchange_reply(&response, (self.clock)())
        })
    }

    fn verify<'a>(
        &'a self,
        login: UnverifiedLogin,
        client_id: &'a str,
        nonce: &'a str,
    ) -> BoxFuture<'a, Result<(format::Account, format::Tokens), LlmError>> {
        Box::pin(async move {
            verify_login(
                login,
                client_id,
                nonce,
                self.discovery(),
                |uri| async move { self.jwks(&uri).await },
            )
            .await
        })
    }

    fn refresh<'a>(
        &'a self,
        client_id: &'a str,
        previous: &'a format::Tokens,
    ) -> BoxFuture<'a, Result<format::Tokens, RefreshFailure>> {
        Box::pin(async move {
            let grant = RefreshGrant {
                client_id,
                refresh_token: &previous.refresh_token.0,
            };
            let response = self
                .request(TOKEN_URL, Some(refresh_form(&grant)))
                .await
                .map_err(|error| RefreshFailure::Other(phase_error(error, "token refresh")))?;
            refresh_reply(&response, previous, (self.clock)())
        })
    }

    fn revoke<'a>(
        &'a self,
        client_id: &'a str,
        refresh_token: &'a str,
    ) -> BoxFuture<'a, Result<(), LlmError>> {
        Box::pin(async move {
            let grant = RefreshGrant {
                client_id,
                refresh_token,
            };
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

impl HttpTokenEndpoint {
    fn discovery(&self) -> BoxFuture<'_, Result<url::Url, LlmError>> {
        Box::pin(async move {
            let response = self
                .request(DISCOVERY_URL, None)
                .await
                .map_err(|error| phase_error(error, "JWKS discovery"))?;
            if response.status != 200 {
                return Err(endpoint_error(
                    &response,
                    "JWKS discovery",
                    false,
                    (self.clock)(),
                ));
            }
            discovery_endpoint(&response.body, "jwks_uri")
        })
    }

    fn jwks<'a>(
        &'a self,
        uri: &'a url::Url,
    ) -> BoxFuture<'a, Result<jsonwebtoken::jwk::JwkSet, LlmError>> {
        Box::pin(async move {
            trusted_endpoint(uri)?;
            let response = self
                .request(uri.as_str(), None)
                .await
                .map_err(|error| phase_error(error, "JWKS retrieval"))?;
            if response.status != 200 {
                return Err(endpoint_error(
                    &response,
                    "JWKS retrieval",
                    false,
                    (self.clock)(),
                ));
            }
            serde_json::from_slice(&response.body)
                .map_err(|_| invalid_response("invalid JWKS document"))
        })
    }
}

fn encoded_form(pairs: &[(&str, &str)]) -> Zeroizing<Vec<u8>> {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.extend_pairs(pairs.iter().copied());
    Zeroizing::new(form.finish().into_bytes())
}

fn code_form(grant: &CodeGrant<'_>) -> Zeroizing<Vec<u8>> {
    encoded_form(&[
        ("grant_type", "authorization_code"),
        ("client_id", grant.client_id),
        ("code", grant.code),
        ("code_verifier", grant.verifier),
        ("redirect_uri", grant.redirect_uri),
        ("resource", "https://api.openai.com/v1"),
    ])
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
    discovery_endpoint(body, "revocation_endpoint")
}

fn discovery_endpoint(body: &[u8], field: &str) -> Result<url::Url, LlmError> {
    #[derive(Deserialize)]
    struct Discovery {
        issuer: String,
        #[serde(flatten)]
        endpoints: std::collections::BTreeMap<String, serde_json::Value>,
    }
    let discovery: Discovery =
        serde_json::from_slice(body).map_err(|_| invalid_response("invalid OpenID discovery"))?;
    if discovery.issuer != ISSUER {
        return Err(configuration("untrusted discovery issuer"));
    }
    let endpoint = discovery
        .endpoints
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| configuration("missing discovery endpoint"))?;
    let url = url::Url::parse(endpoint).map_err(|_| configuration("invalid discovery endpoint"))?;
    trusted_endpoint(&url)?;
    Ok(url)
}

fn trusted_endpoint(url: &url::Url) -> Result<(), LlmError> {
    if url.origin().ascii_serialization() != ISSUER
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(configuration("untrusted discovery endpoint"));
    }
    Ok(())
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

fn parse_login_token(body: &[u8], now: DateTime<Utc>) -> Result<format::Tokens, LlmError> {
    #[derive(Deserialize)]
    struct IdentityToken {
        id_token: Secret,
    }
    let identity: IdentityToken = serde_json::from_slice(body)
        .map_err(|_| invalid_response("missing or invalid ID token"))?;
    if identity.id_token.0.is_empty() || identity.id_token.0.chars().any(char::is_control) {
        return Err(invalid_response("missing or invalid ID token"));
    }
    Ok(parse_token(body, now)?.with_identity(identity.id_token))
}

fn oauth_code(body: &[u8]) -> Option<&'static str> {
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
    let parsed: Option<ErrorReply> = serde_json::from_slice(body).ok();
    let code = parsed.as_ref().map(|reply| match &reply.error {
        Code::Plain(code) | Code::Object { code } => code.0.as_str(),
    });
    match code {
        Some("invalid_grant") => Some("invalid_grant"),
        Some("invalid_refresh_token") => Some("invalid_refresh_token"),
        Some("token_expired") => Some("token_expired"),
        Some("refresh_token_expired") => Some("refresh_token_expired"),
        Some("refresh_token_invalidated") => Some("refresh_token_invalidated"),
        Some("refresh_token_reused") => Some("refresh_token_reused"),
        Some("invalid_client") => Some("invalid_client"),
        _ => None,
    }
}

fn refresh_error(reply: &HttpReply, now: DateTime<Utc>) -> RefreshFailure {
    let code = oauth_code(&reply.body);
    let terminal = code.filter(|code| *code != "invalid_client");
    if let Some(code) = terminal {
        return RefreshFailure::InvalidToken(code);
    }
    let invalid_client = code == Some("invalid_client");
    RefreshFailure::Other(endpoint_error(reply, "token refresh", invalid_client, now))
}

fn endpoint_error(
    reply: &HttpReply,
    action: &str,
    invalid_client: bool,
    now: DateTime<Utc>,
) -> LlmError {
    let mut message = format!("ChatGPT {action} failed: HTTP {}", reply.status);
    if invalid_client {
        message.push_str(
            " invalid_client; run lattice auth remove chatgpt, then lattice auth login chatgpt",
        );
    }
    if let Some(id) = transport::provider_request_id(&reply.headers) {
        message.push_str("; request_id=");
        message.push_str(&id);
    }
    LlmError::ProviderHttp {
        status_code: reply.status,
        message,
        retryable: !invalid_client
            && (reply.status >= 500 || matches!(reply.status, 408 | 409 | 429)),
        retry_after: transport::retry_after(&reply.headers, now.into()),
    }
}

fn configuration(message: &str) -> LlmError {
    LlmError::Config {
        message: format!("ChatGPT OAuth: {message}"),
    }
}

fn phase_error(error: LlmError, phase: &str) -> LlmError {
    crate::client::redact_error_with(error, |message| format!("ChatGPT {phase}: {message}"))
}

fn invalid_response(message: &str) -> LlmError {
    LlmError::Other(format!("ChatGPT OAuth failed: {message}"))
}

fn timeout_error() -> LlmError {
    LlmError::Timeout {
        message: "ChatGPT OAuth did not finish within 30 seconds".into(),
    }
}

fn network_error(error: reqwest::Error) -> LlmError {
    if error.is_timeout() {
        timeout_error()
    } else {
        LlmError::Transport {
            message: "ChatGPT OAuth network request failed".into(),
            retryable: error.is_connect() || error.is_request() || error.is_body(),
        }
    }
}

fn exchange_reply(
    response: &HttpReply,
    now: DateTime<Utc>,
) -> Result<UnverifiedLogin, ExchangeFailure> {
    if response.status != 200 {
        if oauth_code(&response.body) == Some("invalid_grant") {
            return Err(ExchangeFailure::InvalidGrant);
        }
        return Err(ExchangeFailure::Other(endpoint_error(
            response,
            "code exchange",
            oauth_code(&response.body) == Some("invalid_client"),
            now,
        )));
    }
    parse_login_token(&response.body, now)
        .map(|tokens| UnverifiedLogin { tokens })
        .map_err(|error| ExchangeFailure::Other(phase_error(error, "code exchange")))
}

fn refresh_reply(
    response: &HttpReply,
    previous: &format::Tokens,
    now: DateTime<Utc>,
) -> Result<format::Tokens, RefreshFailure> {
    if response.status != 200 {
        return Err(refresh_error(response, now));
    }
    parse_token(&response.body, now)
        .map(|reply| reply.with_identity(Secret(previous.id_token.0.clone())))
        .map_err(|error| RefreshFailure::Other(phase_error(error, "token refresh")))
}

async fn verify_login<F, Fut>(
    login: UnverifiedLogin,
    client_id: &str,
    nonce: &str,
    discovery: impl Future<Output = Result<url::Url, LlmError>>,
    jwks: F,
) -> Result<(format::Account, format::Tokens), LlmError>
where
    F: FnMut(url::Url) -> Fut,
    Fut: Future<Output = Result<JwkSet, LlmError>>,
{
    let identity = verify(&login.tokens.id_token.0, client_id, nonce, discovery, jwks).await?;
    Ok((identity, login.tokens))
}

async fn verify<F, Fut>(
    token: &str,
    client_id: &str,
    nonce: &str,
    discovery: impl Future<Output = Result<url::Url, LlmError>>,
    mut jwks: F,
) -> Result<format::Account, LlmError>
where
    F: FnMut(url::Url) -> Fut,
    Fut: Future<Output = Result<JwkSet, LlmError>>,
{
    let header =
        jsonwebtoken::decode_header(token).map_err(|_| invalid_identity("invalid JWT header"))?;
    if header.alg != Algorithm::RS256 {
        return Err(invalid_identity("only RS256 ID tokens are accepted"));
    }
    let kid = header
        .kid
        .filter(|kid| !kid.is_empty())
        .ok_or_else(|| invalid_identity("missing key ID"))?;
    let uri = discovery.await?;
    let mut keys = jwks(uri.clone()).await?;
    if !keys
        .keys
        .iter()
        .any(|key| key.common.key_id.as_deref() == Some(&kid))
    {
        keys = jwks(uri.clone()).await?;
    }
    verify_with_keys(token, client_id, nonce, &kid, &keys)
}

fn verify_with_keys(
    token: &str,
    client_id: &str,
    nonce: &str,
    kid: &str,
    keys: &JwkSet,
) -> Result<format::Account, LlmError> {
    let mut matching = keys
        .keys
        .iter()
        .filter(|key| key.common.key_id.as_deref() == Some(kid));
    let key = matching
        .next()
        .ok_or_else(|| invalid_identity("unknown key ID"))?;
    if matching.next().is_some() {
        return Err(invalid_identity("ambiguous key ID"));
    }
    validate_identity_key(key)?;
    let decoding = DecodingKey::from_jwk(key).map_err(|_| invalid_identity("invalid RSA key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 5;
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["iss", "aud", "sub", "exp"]);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    let claims = jsonwebtoken::decode::<Claims>(token, &decoding, &validation)
        .map_err(|_| invalid_identity("signature or claims validation failed"))?
        .claims;
    if claims.sub.0.is_empty() || claims.email.is_empty() || claims.nonce.0 != nonce {
        return Err(invalid_identity("invalid identity or nonce"));
    }
    Ok(format::Account {
        subject: claims.sub,
        email: claims.email,
    })
}

fn validate_identity_key(key: &Jwk) -> Result<(), LlmError> {
    if !matches!(key.algorithm, AlgorithmParameters::RSA(_))
        || key
            .common
            .key_algorithm
            .is_some_and(|alg| alg != KeyAlgorithm::RS256)
        || key
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| *usage != PublicKeyUse::Signature)
        || key
            .common
            .key_operations
            .as_ref()
            .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err(invalid_identity("key is not an RS256 signing key"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Claims {
    #[serde(rename = "iss")]
    _issuer: Secret,
    sub: Secret,
    nonce: Secret,
    email: String,
    #[serde(rename = "exp")]
    _expiry: u64,
    #[serde(rename = "nbf", default)]
    _not_before: Option<u64>,
}

fn invalid_identity(reason: &str) -> LlmError {
    LlmError::Credential {
        message: format!("ChatGPT ID token: {reason}"),
    }
}

#[cfg(test)]
pub(super) mod identity_tests;
#[cfg(test)]
pub(super) mod tests;
