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
        ALLOWED_UPDATES, OffsetTracker, RawUpdateEnvelope, TelegramApiError, TelegramIo,
        TelegramUpdate, chat_id_from_key,
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
                let pinned =
                    net_guard::resolve_public_socket_addrs(TELEGRAM_API_HOST, TELEGRAM_API_PORT)
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

#[derive(Deserialize)]
struct RawActionEnvelope {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
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
        let envelope: RawUpdateEnvelope =
            serde_json::from_slice(&payload_bytes).map_err(|_| TelegramApiError::Retryable {
                diagnostic: "解析 Telegram getUpdates 响应 JSON 失败".into(),
            })?;
        if !envelope.ok {
            let desc = envelope
                .description
                .as_deref()
                .map(|d| self.redactor.sanitize_single_line(d))
                .unwrap_or_else(|| "ok=false".into());
            return Err(TelegramApiError::Retryable {
                diagnostic: format!("Telegram getUpdates 返回非成功包：{desc}"),
            });
        }

        {
            let mut offsets = self.offsets.lock().expect("Telegram offset 锁不应中毒");
            for raw in &envelope.result {
                offsets.record_received(raw.update_id);
            }
        }

        Ok(envelope
            .result
            .into_iter()
            .map(|u| u.into_domain())
            .collect())
    }

    fn acknowledge(&self, inbound_id: &InboundId) {
        self.offsets
            .lock()
            .expect("Telegram offset 锁不应中毒")
            .ack_inbound(inbound_id);
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
}
