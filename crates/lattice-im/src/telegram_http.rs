// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 真实 Telegram Bot API HTTPS 客户端适配器：运行期校验并固定公网地址、使用启动期 TLS 信任根并脱敏全部错误。

use std::{sync::Mutex as StdMutex, time::Duration};

use lattice_server::ImToken;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

use crate::{
    identity::{ChatKey, InboundId},
    redact::ImRedactor,
    telegram::{
        ALLOWED_UPDATES, OffsetTracker, RawUpdateEnvelope, TelegramApiError, TelegramDownloadError,
        TelegramIo, TelegramUpdate, chat_id_from_key,
    },
};

const TELEGRAM_API_BASE_URL: &str = "https://api.telegram.org";
const TELEGRAM_API_HOST: &str = "api.telegram.org";
const TELEGRAM_API_PORT: u16 = 443;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// 真实 Telegram Bot API HTTPS 收发实现。
///
/// 构造时不解析 DNS 也不建立连接；首次发起请求时在阻塞线程池中执行 `net_guard` 校验与公网地址固定。
#[cfg(test)]
type BlockingBuildHook = Box<dyn FnOnce() + Send + 'static>;

pub(crate) struct TelegramHttpClient {
    token: ImToken,
    tls_roots_der: Vec<Vec<u8>>,
    redactor: ImRedactor,
    offsets: StdMutex<OffsetTracker>,
    client: Mutex<Option<reqwest::Client>>,
    blocking_builds: StdMutex<Vec<JoinHandle<()>>>,
    #[cfg(test)]
    blocking_hook: StdMutex<Option<BlockingBuildHook>>,
}

impl TelegramHttpClient {
    pub(crate) fn new(token: ImToken, tls_roots_der: Vec<Vec<u8>>, redactor: ImRedactor) -> Self {
        Self {
            token,
            tls_roots_der,
            redactor,
            offsets: StdMutex::new(OffsetTracker::default()),
            client: Mutex::new(None),
            blocking_builds: StdMutex::new(Vec::new()),
            #[cfg(test)]
            blocking_hook: StdMutex::new(None),
        }
    }

    async fn get_or_build_client(&self) -> Result<reqwest::Client, TelegramApiError> {
        let mut guard = self.client.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }

        let mut roots = Vec::with_capacity(self.tls_roots_der.len());
        for der in &self.tls_roots_der {
            if let Ok(cert) = reqwest::Certificate::from_der(der) {
                roots.push(cert);
            }
        }
        if roots.is_empty() {
            return Err(TelegramApiError::Retryable {
                diagnostic: "缺少可用的 TLS 信任根证书，拒绝发起 Telegram HTTPS 请求".into(),
            });
        }

        let redactor = self.redactor.clone();
        #[cfg(test)]
        let test_hook = self
            .blocking_hook
            .lock()
            .expect("测试阻塞钩子锁不应中毒")
            .take();

        let (tx, rx) = oneshot::channel();
        let handle = tokio::task::spawn_blocking(move || {
            lattice_server::with_component_panic_scope(|| {
                #[cfg(test)]
                if let Some(hook) = test_hook {
                    hook();
                }
                let result = (|| -> Result<reqwest::Client, TelegramApiError> {
                    net_guard::validate_base_url(TELEGRAM_API_BASE_URL).map_err(|err| {
                        TelegramApiError::Retryable {
                            diagnostic: format!(
                                "Telegram 端点地址校验未通过：{}",
                                redactor.sanitize_single_line(&err.to_string())
                            ),
                        }
                    })?;
                    let pinned = net_guard::resolve_public_socket_addrs(
                        TELEGRAM_API_HOST,
                        TELEGRAM_API_PORT,
                    )
                    .map_err(|err| TelegramApiError::Retryable {
                        diagnostic: format!(
                            "Telegram 端点公网地址解析失败：{}",
                            redactor.sanitize_single_line(&err.to_string())
                        ),
                    })?;
                    reqwest::Client::builder()
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .tls_certs_only(roots)
                        .connect_timeout(CONNECT_TIMEOUT)
                        .resolve_to_addrs(TELEGRAM_API_HOST, &pinned)
                        .build()
                        .map_err(|err| TelegramApiError::Retryable {
                            diagnostic: format!(
                                "构造 Telegram HTTPS 客户端失败：{}",
                                redactor.redact_reqwest_error(err)
                            ),
                        })
                })();
                let _ = tx.send(result);
            });
        });

        {
            let mut builds = self
                .blocking_builds
                .lock()
                .expect("Telegram 阻塞构建任务锁不应中毒");
            builds.retain(|h| !h.is_finished());
            builds.push(handle);
        }

        let built = rx.await.map_err(|_| TelegramApiError::Retryable {
            diagnostic: "Telegram 地址解析阻塞任务异常中断".into(),
        })??;

        *guard = Some(built.clone());
        Ok(built)
    }

    async fn invalidate_client(&self) {
        let mut guard = self.client.lock().await;
        *guard = None;
    }

    fn method_url(&self, method: &str) -> String {
        format!(
            "{TELEGRAM_API_BASE_URL}/bot{}/{method}",
            self.token.as_str()
        )
    }

    async fn handle_transport_error(&self, error: reqwest::Error) -> TelegramApiError {
        self.invalidate_client().await;
        TelegramApiError::Retryable {
            diagnostic: self.redactor.redact_reqwest_error(error),
        }
    }
}

#[derive(Serialize)]
struct GetUpdatesRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u32,
    allowed_updates: &'a [&'static str],
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    chat_id: i64,
    text: &'a str,
}

#[derive(Serialize)]
struct SendMessageDraftRequest<'a> {
    chat_id: i64,
    draft_id: i64,
    text: &'a str,
    can_stop: bool,
}

#[derive(Serialize)]
struct GetFileRequest<'a> {
    file_id: &'a str,
}

#[derive(Deserialize)]
struct RawActionEnvelope {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawGetFileEnvelope {
    ok: bool,
    #[serde(default)]
    result: Option<RawTelegramFile>,
    #[serde(default)]
    error_code: Option<i64>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Deserialize)]
struct RawTelegramFile {
    #[serde(default)]
    file_size: Option<u64>,
    #[serde(default)]
    file_path: Option<String>,
}

fn validate_file_path(file_path: &str) -> Result<Vec<&str>, TelegramDownloadError> {
    if file_path.is_empty()
        || file_path.starts_with('/')
        || file_path
            .chars()
            .any(|ch| matches!(ch, '\\' | '?' | '#') || ch.is_control())
    {
        return Err(TelegramDownloadError::InvalidFilePath);
    }
    let mut segments = Vec::new();
    for segment in file_path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(TelegramDownloadError::InvalidFilePath);
        }
        segments.push(segment);
    }
    Ok(segments)
}

fn build_bot_method_url(
    base_url: &str,
    token: &str,
    method: &str,
) -> Result<reqwest::Url, TelegramDownloadError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| {
        TelegramDownloadError::Api(TelegramApiError::Retryable {
            diagnostic: "解析 Telegram API 基础地址失败".into(),
        })
    })?;
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: "Telegram API 基础地址不支持路径分段".into(),
            })
        })?;
        segments.push(&format!("bot{token}"));
        segments.push(method);
    }
    Ok(url)
}

fn build_file_download_url(
    base_url: &str,
    token: &str,
    path_segments: &[&str],
) -> Result<reqwest::Url, TelegramDownloadError> {
    let mut url = reqwest::Url::parse(base_url).map_err(|_| {
        TelegramDownloadError::Api(TelegramApiError::Retryable {
            diagnostic: "解析 Telegram API 基础地址失败".into(),
        })
    })?;
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: "Telegram API 基础地址不支持路径分段".into(),
            })
        })?;
        segments.push("file");
        segments.push(&format!("bot{token}"));
        for seg in path_segments {
            segments.push(seg);
        }
    }
    Ok(url)
}

#[derive(Deserialize)]
struct RawTelegramErrorBody {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    parameters: Option<RawResponseParameters>,
}

#[derive(Deserialize)]
struct RawResponseParameters {
    #[serde(default)]
    retry_after: Option<u64>,
}

/// 将非 2xx HTTP 状态码、响应头与错误响应体归类为脱敏后的 `TelegramApiError`。
pub(crate) fn classify_http_failure(
    status: u16,
    retry_after_header: Option<&str>,
    body: &[u8],
    redactor: &ImRedactor,
) -> TelegramApiError {
    let parsed = serde_json::from_slice::<RawTelegramErrorBody>(body).ok();
    let raw_desc = parsed
        .as_ref()
        .and_then(|b| b.description.as_deref())
        .unwrap_or("");
    let cleaned_desc = redactor.sanitize_single_line(raw_desc);
    let diagnostic = if cleaned_desc.is_empty() {
        format!("Telegram 返回 HTTP {status}")
    } else {
        format!("Telegram 返回 HTTP {status}：{cleaned_desc}")
    };

    if status == 429 {
        let body_retry = parsed
            .as_ref()
            .and_then(|b| b.parameters.as_ref())
            .and_then(|p| p.retry_after);
        let header_retry = retry_after_header.and_then(|v| v.trim().parse::<u64>().ok());
        let retry_secs = body_retry.or(header_retry);
        return TelegramApiError::RateLimited {
            retry_after: retry_secs.map(Duration::from_secs),
            diagnostic,
        };
    }
    if (500..=599).contains(&status) {
        return TelegramApiError::Retryable { diagnostic };
    }
    if matches!(status, 400 | 403) {
        return TelegramApiError::Rejected { status, diagnostic };
    }
    TelegramApiError::UnauthorizedOrInvalid { status, diagnostic }
}

async fn read_bounded_bytes(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, reqwest::Error> {
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = limit.saturating_sub(out.len());
        if remaining == 0 {
            break;
        }
        let take = chunk.len().min(remaining);
        out.extend_from_slice(&chunk[..take]);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

impl TelegramHttpClient {
    async fn post_and_read_success_bytes<B: Serialize + Sync>(
        &self,
        method: &str,
        timeout: Duration,
        body: &B,
    ) -> Result<Vec<u8>, TelegramApiError> {
        let client = self.get_or_build_client().await?;
        let response = match client
            .post(self.method_url(method))
            .timeout(timeout)
            .json(body)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => return Err(self.handle_transport_error(err).await),
        };

        let status = response.status();
        if !status.is_success() {
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let err_bytes = match read_bounded_bytes(response, MAX_ERROR_BODY_BYTES).await {
                Ok(b) => b,
                Err(err) => return Err(self.handle_transport_error(err).await),
            };
            return Err(classify_http_failure(
                status.as_u16(),
                retry_header.as_deref(),
                &err_bytes,
                &self.redactor,
            ));
        }

        match response.bytes().await {
            Ok(b) => Ok(b.to_vec()),
            Err(err) => Err(self.handle_transport_error(err).await),
        }
    }

    async fn post_unit_action<B: Serialize + Sync>(
        &self,
        method: &str,
        body: &B,
    ) -> Result<(), TelegramApiError> {
        let payload_bytes = self
            .post_and_read_success_bytes(method, ACTION_TIMEOUT, body)
            .await?;
        let envelope: RawActionEnvelope =
            serde_json::from_slice(&payload_bytes).map_err(|_| TelegramApiError::Retryable {
                diagnostic: format!("解析 Telegram {method} 响应 JSON 失败"),
            })?;
        if !envelope.ok {
            let desc = envelope
                .description
                .as_deref()
                .map(|d| self.redactor.sanitize_single_line(d))
                .unwrap_or_else(|| "ok=false".into());
            return Err(TelegramApiError::Retryable {
                diagnostic: format!("Telegram {method} 返回非成功包：{desc}"),
            });
        }
        Ok(())
    }

    async fn download_file_with_base(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        file_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, TelegramDownloadError> {
        let get_file_url = build_bot_method_url(base_url, self.token.as_str(), "getFile")?;
        let meta_resp = match client
            .post(get_file_url)
            .timeout(ACTION_TIMEOUT)
            .json(&GetFileRequest { file_id })
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                return Err(TelegramDownloadError::Api(
                    self.handle_transport_error(err).await,
                ));
            }
        };

        let meta_status = meta_resp.status();
        let meta_retry_after = meta_resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let meta_bytes = match read_bounded_bytes(meta_resp, MAX_ERROR_BODY_BYTES).await {
            Ok(bytes) => bytes,
            Err(err) => {
                return Err(TelegramDownloadError::Api(
                    self.handle_transport_error(err).await,
                ));
            }
        };

        if !meta_status.is_success() {
            return Err(TelegramDownloadError::Api(classify_http_failure(
                meta_status.as_u16(),
                meta_retry_after.as_deref(),
                &meta_bytes,
                &self.redactor,
            )));
        }

        let envelope: RawGetFileEnvelope = serde_json::from_slice(&meta_bytes).map_err(|_| {
            TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: "解析 Telegram getFile 响应 JSON 失败".into(),
            })
        })?;

        if !envelope.ok {
            if let Some(code) = envelope
                .error_code
                .and_then(|c| u16::try_from(c).ok())
                .filter(|&c| c >= 400)
            {
                return Err(TelegramDownloadError::Api(classify_http_failure(
                    code,
                    meta_retry_after.as_deref(),
                    &meta_bytes,
                    &self.redactor,
                )));
            }
            let desc = envelope
                .description
                .as_deref()
                .map(|d| self.redactor.sanitize_single_line(d))
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| "ok=false".into());
            return Err(TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: format!("Telegram getFile 返回非成功包：{desc}"),
            }));
        }

        let Some(file_meta) = envelope.result else {
            return Err(TelegramDownloadError::InvalidFilePath);
        };
        if file_meta
            .file_size
            .is_some_and(|size| size > max_bytes as u64)
        {
            return Err(TelegramDownloadError::TooLarge);
        }
        let Some(raw_file_path) = file_meta.file_path.as_deref() else {
            return Err(TelegramDownloadError::InvalidFilePath);
        };
        let path_segments = validate_file_path(raw_file_path)?;

        let download_url = build_file_download_url(base_url, self.token.as_str(), &path_segments)?;
        let file_resp = match client
            .get(download_url)
            .timeout(ACTION_TIMEOUT)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(err) => {
                return Err(TelegramDownloadError::Api(
                    self.handle_transport_error(err).await,
                ));
            }
        };

        let file_status = file_resp.status();
        if !file_status.is_success() {
            let retry_after = file_resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let err_bytes = match read_bounded_bytes(file_resp, MAX_ERROR_BODY_BYTES).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    return Err(TelegramDownloadError::Api(
                        self.handle_transport_error(err).await,
                    ));
                }
            };
            return Err(TelegramDownloadError::Api(classify_http_failure(
                file_status.as_u16(),
                retry_after.as_deref(),
                &err_bytes,
                &self.redactor,
            )));
        }

        if file_resp
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(TelegramDownloadError::TooLarge);
        }

        let bytes = match read_bounded_bytes(file_resp, max_bytes.saturating_add(1)).await {
            Ok(bytes) => bytes,
            Err(err) => {
                return Err(TelegramDownloadError::Api(
                    self.handle_transport_error(err).await,
                ));
            }
        };
        if bytes.len() > max_bytes {
            return Err(TelegramDownloadError::TooLarge);
        }
        Ok(bytes)
    }
}

#[async_trait::async_trait]
impl TelegramIo for TelegramHttpClient {
    async fn get_updates(
        &self,
        timeout_secs: u32,
    ) -> Result<Vec<TelegramUpdate>, TelegramApiError> {
        let offset = self
            .offsets
            .lock()
            .expect("Telegram offset 锁不应中毒")
            .next_offset();
        let poll_timeout = Duration::from_secs(u64::from(timeout_secs).saturating_add(20));
        let body = GetUpdatesRequest {
            offset,
            timeout: timeout_secs,
            allowed_updates: &ALLOWED_UPDATES,
        };

        let payload_bytes = self
            .post_and_read_success_bytes("getUpdates", poll_timeout, &body)
            .await?;
        decode_and_filter_updates(&payload_bytes, &self.offsets, &self.redactor)
    }

    fn acknowledge(&self, inbound_id: &InboundId) {
        self.offsets
            .lock()
            .expect("Telegram offset 锁不应中毒")
            .ack_inbound(inbound_id);
    }

    fn is_acknowledged(&self, inbound_id: &InboundId) -> bool {
        self.offsets
            .lock()
            .expect("Telegram offset 锁不应中毒")
            .is_acked_inbound(inbound_id)
    }

    async fn send_message(&self, chat: &ChatKey, text: &str) -> Result<(), TelegramApiError> {
        let chat_id = chat_id_from_key(chat).map_err(|reason| TelegramApiError::Rejected {
            status: 400,
            diagnostic: reason.to_owned(),
        })?;
        let body = SendMessageRequest { chat_id, text };
        self.post_unit_action("sendMessage", &body).await
    }

    async fn send_message_draft(
        &self,
        chat: &ChatKey,
        draft_id: i64,
        text: &str,
        can_stop: bool,
    ) -> Result<(), TelegramApiError> {
        let chat_id = chat_id_from_key(chat).map_err(|reason| TelegramApiError::Rejected {
            status: 400,
            diagnostic: reason.to_owned(),
        })?;
        let body = SendMessageDraftRequest {
            chat_id,
            draft_id,
            text,
            can_stop,
        };
        self.post_unit_action("sendMessageDraft", &body).await
    }

    async fn download_file(
        &self,
        file_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, TelegramDownloadError> {
        let client = self
            .get_or_build_client()
            .await
            .map_err(TelegramDownloadError::Api)?;
        self.download_file_with_base(&client, TELEGRAM_API_BASE_URL, file_id, max_bytes)
            .await
    }

    async fn drain(&self) {
        let handles = {
            let mut builds = self
                .blocking_builds
                .lock()
                .expect("Telegram 阻塞构建任务锁不应中毒");
            std::mem::take(&mut *builds)
        };
        for handle in handles {
            let _ = handle.await;
        }
    }
}

fn decode_and_filter_updates(
    payload_bytes: &[u8],
    offsets: &StdMutex<OffsetTracker>,
    redactor: &ImRedactor,
) -> Result<Vec<TelegramUpdate>, TelegramApiError> {
    let envelope: RawUpdateEnvelope =
        serde_json::from_slice(payload_bytes).map_err(|_| TelegramApiError::Retryable {
            diagnostic: "解析 Telegram getUpdates 响应 JSON 失败".into(),
        })?;
    if !envelope.ok {
        let desc = envelope
            .description
            .as_deref()
            .map(|d| redactor.sanitize_single_line(d))
            .unwrap_or_else(|| "ok=false".into());
        return Err(TelegramApiError::Retryable {
            diagnostic: format!("Telegram getUpdates 返回非成功包：{desc}"),
        });
    }

    let mut tracker = offsets.lock().expect("Telegram offset 锁不应中毒");
    let mut seen_in_batch = std::collections::BTreeSet::new();
    Ok(envelope
        .result
        .into_iter()
        .filter(|raw| seen_in_batch.insert(raw.update_id) && tracker.record_received(raw.update_id))
        .map(|u| u.into_domain())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_429_5xx_400_403_and_401_with_redaction() {
        let token = ImToken::parse("secret-bot-token").unwrap();
        let redactor = ImRedactor::new([token]);

        let rate_body = br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry secret-bot-token","parameters":{"retry_after":7}}"#;
        let rate_err = classify_http_failure(429, Some("3"), rate_body, &redactor);
        assert_eq!(
            rate_err,
            TelegramApiError::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
                diagnostic: "Telegram 返回 HTTP 429：Too Many Requests: retry [REDACTED]".into(),
            }
        );

        let header_fallback_err = classify_http_failure(429, Some("12"), b"{}", &redactor);
        assert_eq!(
            header_fallback_err,
            TelegramApiError::RateLimited {
                retry_after: Some(Duration::from_secs(12)),
                diagnostic: "Telegram 返回 HTTP 429".into(),
            }
        );

        let server_err = classify_http_failure(502, None, b"bad gateway", &redactor);
        assert!(matches!(server_err, TelegramApiError::Retryable { .. }));

        for status in [400, 403] {
            let rejected = classify_http_failure(
                status,
                None,
                br#"{"ok":false,"description":"Forbidden: bot was blocked by the user"}"#,
                &redactor,
            );
            assert!(
                matches!(rejected, TelegramApiError::Rejected { status: s, .. } if s == status),
                "HTTP {status} 应归类为平台拒收终态"
            );
        }

        let unauth = classify_http_failure(
            401,
            None,
            br#"{"ok":false,"description":"Unauthorized secret-bot-token"}"#,
            &redactor,
        );
        assert!(matches!(
            unauth,
            TelegramApiError::UnauthorizedOrInvalid { status: 401, ref diagnostic }
                if !diagnostic.contains("secret-bot-token")
        ));
    }

    #[tokio::test]
    async fn rejects_https_requests_when_tls_roots_are_empty() {
        let token = ImToken::parse("123:test-token").unwrap();
        let redactor = ImRedactor::new([token.clone()]);
        let client = TelegramHttpClient::new(token, Vec::new(), redactor);

        let err = client
            .get_updates(1)
            .await
            .expect_err("空 TLS 根证书集合必须拒绝发起 HTTPS 请求");
        assert!(
            err.diagnostic().contains("TLS 信任根"),
            "错误说明应指出缺少可用 TLS 根证书: {}",
            err.diagnostic()
        );
    }

    #[tokio::test]
    async fn drain_waits_for_in_flight_blocking_setup_after_request_is_cancelled() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        // 构造一张合法自签 ECDSA DER 根证书，使 get_or_build_client 通过前置检查并进入 spawn_blocking
        let sample_der: Vec<u8> = vec![
            48, 130, 1, 116, 48, 130, 1, 25, 160, 3, 2, 1, 2, 2, 20, 33, 97, 84, 84, 47, 177, 1,
            163, 47, 123, 129, 164, 47, 61, 49, 167, 183, 85, 115, 153, 48, 10, 6, 8, 42, 134, 72,
            206, 61, 4, 3, 2, 48, 15, 49, 13, 48, 11, 6, 3, 85, 4, 3, 12, 4, 116, 101, 115, 116,
            48, 30, 23, 13, 50, 54, 49, 49, 49, 52, 50, 51, 53, 49, 51, 48, 90, 23, 13, 51, 54, 49,
            49, 49, 49, 50, 51, 53, 49, 51, 48, 90, 48, 15, 49, 13, 48, 11, 6, 3, 85, 4, 3, 12, 4,
            116, 101, 115, 116, 48, 89, 48, 19, 6, 7, 42, 134, 72, 206, 61, 2, 1, 6, 8, 42, 134,
            72, 206, 61, 3, 1, 7, 3, 66, 0, 4, 175, 141, 1, 119, 152, 135, 224, 133, 230, 62, 147,
            228, 20, 76, 50, 19, 97, 89, 110, 35, 167, 79, 34, 100, 231, 110, 55, 235, 155, 164,
            103, 179, 232, 53, 242, 143, 55, 14, 34, 113, 214, 121, 15, 242, 85, 86, 247, 116, 187,
            37, 56, 76, 91, 184, 205, 91, 25, 159, 198, 45, 106, 251, 109, 50, 163, 83, 48, 81, 48,
            29, 6, 3, 85, 29, 14, 4, 22, 4, 20, 116, 78, 59, 75, 22, 17, 147, 197, 49, 98, 20, 18,
            204, 54, 34, 49, 123, 79, 141, 110, 48, 31, 6, 3, 85, 29, 35, 4, 24, 48, 22, 128, 20,
            116, 78, 59, 75, 22, 17, 147, 197, 49, 98, 20, 18, 204, 54, 34, 49, 123, 79, 141, 110,
            48, 15, 6, 3, 85, 29, 19, 1, 1, 255, 4, 5, 48, 3, 1, 1, 255, 48, 10, 6, 8, 42, 134, 72,
            206, 61, 4, 3, 2, 3, 73, 0, 48, 70, 2, 33, 0, 136, 234, 163, 133, 47, 53, 106, 41, 70,
            206, 234, 253, 217, 239, 246, 199, 14, 158, 237, 221, 14, 93, 35, 58, 239, 57, 233,
            141, 146, 80, 122, 6, 2, 33, 0, 218, 225, 26, 59, 116, 246, 160, 86, 82, 12, 208, 141,
            252, 164, 218, 13, 178, 160, 254, 177, 251, 27, 69, 11, 108, 36, 58, 187, 158, 28, 217,
            103,
        ];
        let token = ImToken::parse("123:test-token").unwrap();
        let redactor = ImRedactor::new([token.clone()]);
        let client = TelegramHttpClient::new(token, vec![sample_der], redactor);

        let (entered_tx, entered_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_in_hook = Arc::clone(&finished);

        *client.blocking_hook.lock().unwrap() = Some(Box::new(move || {
            let _ = entered_tx.send(());
            let _ = release_rx.recv();
            finished_in_hook.store(true, Ordering::SeqCst);
        }));

        {
            let mut req_fut = Box::pin(client.get_updates(1));
            tokio::select! {
                _ = entered_rx => {}
                _ = &mut req_fut => panic!("阻塞钩子释放前 get_updates 不应提前完成"),
            }
            // 丢弃请求 future（模拟收到停止信号中断轮询）
        }
        assert!(
            !finished.load(Ordering::SeqCst),
            "释放阻塞钩子前后台阻塞任务仍应在运行"
        );

        let drain_task = tokio::spawn(async move {
            client.drain().await;
            finished.load(Ordering::SeqCst)
        });
        tokio::task::yield_now().await;
        assert!(
            !drain_task.is_finished(),
            "后台阻塞任务未退出前 drain() 不得提前返回"
        );

        release_tx.send(()).unwrap();
        let saw_finished = drain_task.await.expect("drain 任务不应 panic");
        assert!(saw_finished, "drain() 返回时阻塞任务必须已完成");
    }

    #[test]
    fn decode_and_filter_updates_drops_acknowledged_commands_redelivered_behind_blocked_head() {
        let token = ImToken::parse("123:test-token").unwrap();
        let redactor = ImRedactor::new([token]);
        let offsets = StdMutex::new(OffsetTracker::default());

        let batch1 = r#"{
            "ok": true,
            "result": [
                {"update_id": 50, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "排队中的普通消息"}},
                {"update_id": 51, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "/help"}},
                {"update_id": 52, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "/cancel"}}
            ]
        }"#;
        let first = decode_and_filter_updates(batch1.as_bytes(), &offsets, &redactor).unwrap();
        assert_eq!(first.len(), 3);

        // 模拟 50 仍在排队未确认，51（/help）与 52（/cancel）收到即确认：offset 仍为 None
        {
            let mut guard = offsets.lock().unwrap();
            guard.ack_inbound(&first[1].inbound_id);
            guard.ack_inbound(&first[2].inbound_id);
            assert_eq!(guard.next_offset(), None);
        }

        // 下一轮 getUpdates 重投 [50, 51, 52] 并附带新到达的 53：已确认的 51、52 必须在返回前被过滤掉
        let batch2 = r#"{
            "ok": true,
            "result": [
                {"update_id": 50, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "排队中的普通消息"}},
                {"update_id": 51, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "/help"}},
                {"update_id": 52, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "/cancel"}},
                {"update_id": 53, "message": {"chat": {"id": 1, "type": "private"}, "from": {"id": 9}, "text": "/status"}}
            ]
        }"#;
        let second = decode_and_filter_updates(batch2.as_bytes(), &offsets, &redactor).unwrap();
        let ids: Vec<String> = second.iter().map(|u| u.inbound_id.to_string()).collect();
        assert_eq!(
            ids,
            vec!["tg:50".to_string(), "tg:53".to_string()],
            "前序更新挡住 offset 时，已确认的 /help 与 /cancel 重投不得再次进入网关分发"
        );
    }

    #[test]
    fn validates_telegram_file_path_against_traversal_delimiters_and_control_chars() {
        assert_eq!(
            validate_file_path("file_0.jpg"),
            Ok(vec!["file_0.jpg"]),
            "单段合法相对路径应通过校验"
        );
        assert_eq!(
            validate_file_path("photos/sub_dir/file_12.png"),
            Ok(vec!["photos", "sub_dir", "file_12.png"]),
            "多段合法相对路径应按斜杠分段通过校验"
        );

        let rejected = [
            "",
            "/",
            "/photos/a.jpg",
            "photos/a.jpg/",
            "photos//a.jpg",
            ".",
            "..",
            "photos/./a.jpg",
            "photos/../bot123/getMe",
            "../a.jpg",
            "photos/..",
            "photos\\a.jpg",
            "photos/a.jpg?token=leak",
            "photos/a.jpg#frag",
            "photos/a\0.jpg",
            "photos/a\n.jpg",
            "photos/a\r.jpg",
            "photos/a\t.jpg",
            "photos/a\x7f.jpg",
        ];
        for bad in rejected {
            assert_eq!(
                validate_file_path(bad),
                Err(TelegramDownloadError::InvalidFilePath),
                "非法 file_path {bad:?} 必须被拒绝"
            );
        }
    }

    async fn read_raw_http_head_and_body(sock: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
        use tokio::io::AsyncReadExt;

        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(sock.read_u8().await.expect("读取 HTTP 请求头应成功"));
        }
        let head_str = String::from_utf8(head).expect("HTTP 请求头应为 UTF-8");
        let request_line = head_str.lines().next().unwrap_or("").to_owned();
        let content_len: usize = head_str
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0);
        let mut body = vec![0u8; content_len];
        if content_len > 0 {
            sock.read_exact(&mut body)
                .await
                .expect("读取 HTTP 请求体应成功");
        }
        (request_line, body)
    }

    #[tokio::test]
    async fn download_file_enforces_size_bounds_aborts_oversized_stream_and_never_leaks_token() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::{io::AsyncWriteExt, net::TcpListener};

        const SECRET_TOKEN: &str = "777888:ULTRA_SECRET_DOWNLOAD_TOKEN";
        let token = ImToken::parse(SECRET_TOKEN).unwrap();
        let redactor = ImRedactor::new([token.clone()]);
        let tg_client = TelegramHttpClient::new(token, Vec::new(), redactor);
        let raw_http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // 1. getFile 声明 file_size 超限时立即短路返回 TooLarge，绝不发起第二次 GET 连接
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let accepted = Arc::new(AtomicUsize::new(0));
            let accepted_srv = Arc::clone(&accepted);
            let srv = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                accepted_srv.fetch_add(1, Ordering::SeqCst);
                let (req_line, req_body) = read_raw_http_head_and_body(&mut sock).await;
                assert_eq!(
                    req_line,
                    format!("POST /bot{SECRET_TOKEN}/getFile HTTP/1.1")
                );
                let parsed: serde_json::Value = serde_json::from_slice(&req_body).unwrap();
                assert_eq!(parsed["file_id"], "oversized_meta");
                let json = r#"{"ok":true,"result":{"file_id":"oversized_meta","file_size":1025,"file_path":"photos/huge.png"}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            });

            let err = tg_client
                .download_file_with_base(&raw_http, &base, "oversized_meta", 1024)
                .await
                .expect_err("声明 file_size 超限必须直接拒绝");
            assert_eq!(err, TelegramDownloadError::TooLarge);
            srv.await.unwrap();
            assert_eq!(
                accepted.load(Ordering::SeqCst),
                1,
                "getFile 声明超限时不得发起第二次 GET 请求"
            );
        }

        // 2. getFile 返回非法 file_path 时立即返回 InvalidFilePath，不发起 GET
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let srv = tokio::spawn(async move {
                let (mut sock, _) = listener.accept().await.unwrap();
                let _ = read_raw_http_head_and_body(&mut sock).await;
                let json = r#"{"ok":true,"result":{"file_id":"bad_path","file_path":"photos/../botleak"}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                sock.write_all(resp.as_bytes()).await.unwrap();
            });

            let err = tg_client
                .download_file_with_base(&raw_http, &base, "bad_path", 1024)
                .await
                .expect_err("路径穿越 file_path 必须拒绝");
            assert_eq!(err, TelegramDownloadError::InvalidFilePath);
            srv.await.unwrap();
        }

        // 3. 恰好等于 max_bytes 的流式下载成功，且核验 GET /file/bot<token>/<file_path> 路径拼接
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let payload = vec![0x5Au8; 1024];
            let payload_srv = payload.clone();
            let srv = tokio::spawn(async move {
                let (mut s1, _) = listener.accept().await.unwrap();
                let _ = read_raw_http_head_and_body(&mut s1).await;
                let json = r#"{"ok":true,"result":{"file_id":"exact_1k","file_size":1024,"file_path":"photos/sub/exact.png"}}"#;
                let r1 = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                s1.write_all(r1.as_bytes()).await.unwrap();
                drop(s1);

                let (mut s2, _) = listener.accept().await.unwrap();
                let (req_line, _) = read_raw_http_head_and_body(&mut s2).await;
                assert_eq!(
                    req_line,
                    format!("GET /file/bot{SECRET_TOKEN}/photos/sub/exact.png HTTP/1.1")
                );
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload_srv.len()
                );
                s2.write_all(head.as_bytes()).await.unwrap();
                s2.write_all(&payload_srv).await.unwrap();
            });

            let got = tg_client
                .download_file_with_base(&raw_http, &base, "exact_1k", 1024)
                .await
                .expect("恰好等于上限的响应体应完整下载成功");
            assert_eq!(got, payload);
            srv.await.unwrap();
        }

        // 4. 无 Content-Length 的 chunked 响应流在读到 max_bytes + 1 字节时立即中断连接，不等待后续流结束
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (_hold_tx, hold_rx) = oneshot::channel::<()>();
            let srv = tokio::spawn(async move {
                let (mut s1, _) = listener.accept().await.unwrap();
                let _ = read_raw_http_head_and_body(&mut s1).await;
                let json = r#"{"ok":true,"result":{"file_id":"stream_overflow","file_path":"photos/stream.png"}}"#;
                let r1 = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    json.len()
                );
                s1.write_all(r1.as_bytes()).await.unwrap();
                drop(s1);

                let (mut s2, _) = listener.accept().await.unwrap();
                let _ = read_raw_http_head_and_body(&mut s2).await;
                s2.write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
                )
                .await
                .unwrap();
                // 写出 513 字节分块（超过 max_bytes = 512 一字节），随后故意挂起不发送结束块
                let over_chunk = vec![b'X'; 513];
                s2.write_all(format!("{:X}\r\n", over_chunk.len()).as_bytes())
                    .await
                    .unwrap();
                s2.write_all(&over_chunk).await.unwrap();
                s2.write_all(b"\r\n").await.unwrap();
                s2.flush().await.unwrap();
                let _ = hold_rx.await;
            });

            let err = tokio::time::timeout(
                Duration::from_secs(3),
                tg_client.download_file_with_base(&raw_http, &base, "stream_overflow", 512),
            )
            .await
            .expect("读取到 max_bytes + 1 后必须立即中断流返回，不得阻塞等待流结束")
            .expect_err("超过上限 1 字节必须返回 TooLarge");
            assert_eq!(err, TelegramDownloadError::TooLarge);
            srv.abort();
        }

        // 5. 状态码分类（400/404/429/5xx，含 HTTP 200 ok=false 带 error_code）与传输异常零凭据泄露
        for (mode, expected_check) in [
            ("http_400", "rejected_400"),
            ("ok_false_404", "unauth_404"),
            ("file_429", "rate_429"),
            ("file_503", "retryable"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let srv = tokio::spawn(async move {
                let (mut s1, _) = listener.accept().await.unwrap();
                let _ = read_raw_http_head_and_body(&mut s1).await;
                match mode {
                    "http_400" => {
                        let body = format!(
                            r#"{{"ok":false,"error_code":400,"description":"Bad Request: invalid file_id {SECRET_TOKEN}"}}"#
                        );
                        let r = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        s1.write_all(r.as_bytes()).await.unwrap();
                    }
                    "ok_false_404" => {
                        let body = format!(
                            r#"{{"ok":false,"error_code":404,"description":"Not Found {SECRET_TOKEN}"}}"#
                        );
                        let r = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        s1.write_all(r.as_bytes()).await.unwrap();
                    }
                    _ => {
                        let json =
                            r#"{"ok":true,"result":{"file_id":"f","file_path":"photos/a.png"}}"#;
                        let r1 = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                            json.len()
                        );
                        s1.write_all(r1.as_bytes()).await.unwrap();
                        drop(s1);

                        let (mut s2, _) = listener.accept().await.unwrap();
                        let _ = read_raw_http_head_and_body(&mut s2).await;
                        if mode == "file_429" {
                            let body = format!(
                                r#"{{"ok":false,"error_code":429,"description":"Too Many Requests {SECRET_TOKEN}","parameters":{{"retry_after":9}}}}"#
                            );
                            let r2 = format!(
                                "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            s2.write_all(r2.as_bytes()).await.unwrap();
                        } else {
                            let body = format!("upstream broken {SECRET_TOKEN}");
                            let r2 = format!(
                                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            s2.write_all(r2.as_bytes()).await.unwrap();
                        }
                    }
                }
            });

            let err = tg_client
                .download_file_with_base(&raw_http, &base, "f", 1024)
                .await
                .expect_err("异常响应必须返回错误");
            srv.await.unwrap();

            let TelegramDownloadError::Api(api_err) = err else {
                panic!("模式 {mode} 应返回 Api 错误，实际: {err:?}");
            };
            assert!(
                !api_err.diagnostic().contains(SECRET_TOKEN),
                "模式 {mode} 错误诊断绝不得泄露机器人凭据: {}",
                api_err.diagnostic()
            );
            match expected_check {
                "rejected_400" => assert!(matches!(
                    api_err,
                    TelegramApiError::Rejected { status: 400, .. }
                )),
                "unauth_404" => assert!(matches!(
                    api_err,
                    TelegramApiError::UnauthorizedOrInvalid { status: 404, .. }
                )),
                "rate_429" => assert!(matches!(
                    api_err,
                    TelegramApiError::RateLimited {
                        retry_after: Some(d),
                        ..
                    } if d == Duration::from_secs(9)
                )),
                "retryable" => assert!(matches!(api_err, TelegramApiError::Retryable { .. })),
                _ => unreachable!(),
            }
        }

        // 6. 连接拒绝与流读取中途断开：传输层 reqwest::Error 经 without_url + ImRedactor 后绝不泄露凭据路径
        {
            let closed_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let closed_addr = closed_listener.local_addr().unwrap();
            drop(closed_listener);
            let closed_base = format!("http://{closed_addr}");

            let err = tg_client
                .download_file_with_base(&raw_http, &closed_base, "conn_refused", 1024)
                .await
                .expect_err("已关闭端口应返回传输层可重试错误");
            match err {
                TelegramDownloadError::Api(TelegramApiError::Retryable { diagnostic }) => {
                    assert!(
                        !diagnostic.contains(SECRET_TOKEN),
                        "连接失败诊断不得泄露凭据: {diagnostic}"
                    );
                    assert!(
                        !diagnostic.contains("/getFile"),
                        "without_url 应剥离含凭据的请求路径: {diagnostic}"
                    );
                }
                other => panic!("连接失败应归类为 Retryable，实际: {other:?}"),
            }
        }
    }
}
