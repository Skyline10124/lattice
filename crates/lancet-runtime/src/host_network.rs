// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! GET-only network authority for the `network-fetch` Tool Host import (#32).

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper::client::conn::http1;
use hyper::http::header::{
    ACCEPT_ENCODING, CONNECTION, CONTENT_ENCODING, CONTENT_TYPE, HOST, LOCATION,
};
use hyper::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use lancet_authority::NetworkPolicyConfig;
use lancet_authority::policy::network::validate_fetch_url;
use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact};
use lancet_protocol::error::{
    CapabilityErrorData, PermissionErrorData, ReservedErrorData, ValidationErrorData,
};
use lancet_protocol::evidence::{
    NetworkEndpoint, NetworkEvidence, NetworkFailurePhase, NetworkPolicyVerdict, NetworkResolution,
    ObservedTarget, TlsState, TlsVersion, TransportEvidence,
};
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::admission::{AdmittedTool, authorize_operation, resolve_scope};
use crate::bindings;
use crate::error_map::cancelled_error;
use crate::evidence::CellEvidenceLedger;
use crate::host_workspace::{BudgetCharge, BudgetRefusal};
use crate::runtime::StoreState;

const METHOD: &str = "fetch__web";
const MAX_REDIRECTS: u32 = 5;
const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_REQUEST_HEADERS: usize = 32;
const MAX_REQUEST_HEADER_NAME_BYTES: usize = 128;
const MAX_REQUEST_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_RESPONSE_HEADERS: usize = 64;
const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;

use bindings::lattice::tool_host::network_fetch::{FetchError, FetchRequest, FetchResponse};

/// Remaining shared observation bytes for the current Turn.
pub type BudgetRemaining = Arc<dyn Fn() -> usize + Send + Sync>;
/// Call-scoped host authority. A Component gets one instance and may call it once.
pub struct NetworkFetchHost {
    policy: NetworkPolicyConfig,
    /// `None` 表示没有可用的 TLS 根：https 请求在解析与连接前拒绝，http 照常。
    tls: Option<Arc<ClientConfig>>,
    charge: BudgetCharge,
    remaining: BudgetRemaining,
    scope_name: String,
    used: AtomicBool,
    refusal: Mutex<Option<CepError>>,
}

impl NetworkFetchHost {
    pub fn new(
        policy: NetworkPolicyConfig,
        charge: BudgetCharge,
        remaining: BudgetRemaining,
        scope_name: impl Into<String>,
        tls: Option<Arc<ClientConfig>>,
    ) -> Self {
        Self {
            policy,
            tls,
            charge,
            remaining,
            scope_name: scope_name.into(),
            used: AtomicBool::new(false),
            refusal: Mutex::new(None),
        }
    }

    pub fn take_refusal(&self) -> Option<CepError> {
        self.refusal.lock().unwrap().take()
    }

    fn remember(&self, error: CepError) {
        let mut refusal = self.refusal.lock().unwrap();
        if refusal.is_none() {
            *refusal = Some(error);
        }
    }

    fn cell(&self) -> Cell {
        Cell {
            ctx_op: CtxOp::Alloc,
            ext_impact: ExtImpact::Read,
            scope: self.scope_name.clone(),
        }
    }

    fn scope(&self, ctx: &NetworkCallContext) -> lancet_protocol::ObservedScope {
        resolve_scope(
            &ctx.admitted.descriptor,
            &self.scope_name,
            ChannelKind::Network,
        )
    }

    async fn fetch(
        &self,
        request: FetchRequest,
        ctx: NetworkCallContext,
    ) -> Result<FetchResponse, FetchError> {
        if self.used.swap(true, Ordering::AcqRel) {
            let error = denied_error("network import may be called only once");
            self.remember(error);
            return Err(FetchError::Denied);
        }

        let caller_headers = match validated_headers(request.headers) {
            Ok(headers) => headers,
            Err(error) => {
                ctx.evidence.record_denied(
                    self.cell(),
                    self.scope(&ctx),
                    ObservedTarget::None,
                    "invalid_request_headers",
                );
                self.remember(error);
                return Err(FetchError::InvalidRequest);
            }
        };

        self.fetch_chain(request.url, caller_headers, &ctx).await
    }

    async fn fetch_chain(
        &self,
        initial_url: String,
        caller_headers: HeaderMap,
        ctx: &NetworkCallContext,
    ) -> Result<FetchResponse, FetchError> {
        let mut current = initial_url;
        let mut redirects = 0u32;
        let mut caller_headers = Some(caller_headers);
        let mut previous_origin: Option<String> = None;

        loop {
            if let Some(wait) = control_state(ctx) {
                return Err(self.wait_refusal(wait));
            }
            let hop = redirects;
            let validated = match self.validate_url(&current, hop, ctx) {
                Ok(validated) => validated,
                Err(error) => return Err(error),
            };
            if hop == 0 && (self.remaining)() == 0 {
                let target = ObservedTarget::Host {
                    host: validated.host.clone(),
                };
                let evidence =
                    empty_network_evidence(hop, &validated.url, NetworkPolicyVerdict::Denied);
                ctx.evidence.record_network_denied(
                    self.cell(),
                    self.scope(ctx),
                    target,
                    "observation_budget_pre_io",
                    evidence,
                );
                self.remember(budget_error(&BudgetRefusal {
                    limit: "observation.max_turn_observation_bytes",
                    requested: 1,
                    remaining: 0,
                }));
                return Err(FetchError::LimitExceeded);
            }
            let origin = validated.url.origin().ascii_serialization();
            if previous_origin
                .as_ref()
                .is_some_and(|previous| previous != &origin)
            {
                caller_headers = None;
            }

            let outcome = self
                .fetch_hop(
                    validated,
                    caller_headers.as_ref().unwrap_or(&HeaderMap::new()),
                    hop,
                    ctx,
                )
                .await?;
            match outcome {
                HopOutcome::Final(response) => return Ok(response),
                HopOutcome::Redirect(next) => {
                    if redirects == MAX_REDIRECTS {
                        let error = limit_error("redirects");
                        self.remember(error);
                        return Err(FetchError::LimitExceeded);
                    }
                    previous_origin = Some(origin);
                    redirects += 1;
                    current = next;
                }
            }
        }
    }

    fn validate_url(
        &self,
        raw: &str,
        hop: u32,
        ctx: &NetworkCallContext,
    ) -> Result<ValidatedUrl, FetchError> {
        let initial = hop == 0;
        if raw.len() > MAX_URL_BYTES {
            let error = if initial {
                invalid_error("url", "URL exceeds the host limit")
            } else {
                limit_error("redirect_url")
            };
            self.remember(error);
            return Err(if initial {
                FetchError::InvalidRequest
            } else {
                FetchError::LimitExceeded
            });
        }

        let mut parsed = match Url::parse(raw) {
            Ok(parsed) => parsed,
            Err(_) => {
                self.remember(if initial {
                    invalid_error("url", "URL is not valid")
                } else {
                    unavailable_error()
                });
                return Err(if initial {
                    FetchError::InvalidRequest
                } else {
                    FetchError::Unavailable
                });
            }
        };
        let host = match parsed.host_str() {
            Some(host) => host.to_string(),
            None => {
                self.remember(invalid_error("url", "URL must include a host"));
                return Err(FetchError::InvalidRequest);
            }
        };
        let target = ObservedTarget::Host { host: host.clone() };

        if !parsed.username().is_empty() || parsed.password().is_some() {
            ctx.evidence
                .record_denied(self.cell(), self.scope(ctx), target, "url_userinfo");
            self.remember(invalid_error("url", "URL userinfo is not allowed"));
            return Err(FetchError::InvalidRequest);
        }
        parsed.set_fragment(None);
        if parsed.as_str().len() > MAX_URL_BYTES {
            self.record_policy_denial(&parsed, &host, hop, ctx, "url_limit");
            self.remember(limit_error("redirect_url"));
            return Err(FetchError::LimitExceeded);
        }

        let validated = match validate_fetch_url(parsed.as_str(), &self.policy) {
            Ok(validated) => validated,
            Err(_) => {
                self.record_policy_denial(&parsed, &host, hop, ctx, "network_policy");
                self.remember(denied_error("network request denied by policy"));
                return Err(FetchError::Denied);
            }
        };

        if let Err(denial) = authorize_operation(&ctx.admitted, &self.cell(), &target, &ctx.turn_id)
        {
            ctx.evidence.record_network_denied(
                self.cell(),
                self.scope(ctx),
                target,
                "cell_denied",
                empty_network_evidence(hop, &parsed, NetworkPolicyVerdict::Denied),
            );
            self.remember(effect_error(denial.to_string()));
            return Err(FetchError::Denied);
        }

        // 放在网络策略与单元授权之后、解析与连接之前：不在允许范围内的目标照常按策略拒绝，
        // 只有本来会被放行的 https 请求才报告缺少根证书。
        // 失败即关闭：没有可用根时只放行 http。URL 已在上面解析成功（解析失败已拒绝），
        // 且 validate_fetch_url 只允许 http/https；这里仍不以 "== https" 判断，避免漏放其他 scheme。
        if parsed.scheme() != "http" && self.tls.is_none() {
            self.record_policy_denial(&parsed, &host, hop, ctx, "tls_roots_unavailable");
            self.remember(denied_error(NO_TLS_ROOTS));
            return Err(FetchError::Denied);
        }

        Ok(ValidatedUrl {
            url: parsed,
            host,
            port: validated.port,
            localhost: validated.use_localhost_client,
        })
    }

    fn record_policy_denial(
        &self,
        url: &Url,
        host: &str,
        hop: u32,
        ctx: &NetworkCallContext,
        reason: &str,
    ) {
        ctx.evidence.record_network_denied(
            self.cell(),
            self.scope(ctx),
            ObservedTarget::Host {
                host: host.to_string(),
            },
            reason,
            empty_network_evidence(hop, url, NetworkPolicyVerdict::Denied),
        );
    }

    async fn fetch_hop(
        &self,
        validated: ValidatedUrl,
        caller_headers: &HeaderMap,
        hop: u32,
        ctx: &NetworkCallContext,
    ) -> Result<HopOutcome, FetchError> {
        let target = ObservedTarget::Host {
            host: validated.host.clone(),
        };
        let mut evidence =
            empty_network_evidence(hop, &validated.url, NetworkPolicyVerdict::Allowed);

        let candidates = match resolve_candidates(&validated, ctx).await {
            Ok(candidates) => candidates,
            Err(wait) => {
                evidence.resolution = NetworkResolution::Failed;
                evidence.failure_phase = Some(NetworkFailurePhase::Dns);
                self.record_effect(ctx, target, evidence, None, None);
                return Err(self.wait_refusal(wait));
            }
        };
        evidence.resolution = if parse_ip_literal(&validated.host).is_some() {
            NetworkResolution::Literal
        } else {
            NetworkResolution::Resolved
        };
        evidence.dns_candidates = candidates.iter().copied().map(endpoint).collect();

        if !candidates_allowed(&validated, &candidates) {
            evidence.policy_verdict = NetworkPolicyVerdict::Denied;
            self.record_effect(ctx, target, evidence, None, None);
            self.remember(denied_error("resolved network target is not allowed"));
            return Err(FetchError::Denied);
        }

        let stream = match connect_candidates(&candidates, ctx).await {
            Ok(stream) => stream,
            Err(wait) => {
                evidence.failure_phase = Some(NetworkFailurePhase::Connect);
                self.record_effect(ctx, target, evidence, None, None);
                return Err(self.wait_refusal(wait));
            }
        };
        let peer = stream
            .peer_addr()
            .ok()
            .or_else(|| candidates.first().copied());
        evidence.transport.connected_peer = peer.map(endpoint);

        let headers = match request_headers(&validated.url, caller_headers) {
            Ok(headers) => headers,
            Err(error) => {
                evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
                self.record_effect(ctx, target, evidence, None, None);
                self.remember(error);
                return Err(FetchError::InvalidRequest);
            }
        };
        let request = match build_request(&validated.url, headers) {
            Ok(request) => request,
            Err(error) => {
                evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
                self.record_effect(ctx, target, evidence, None, None);
                self.remember(error);
                return Err(FetchError::InvalidRequest);
            }
        };

        let response = if validated.url.scheme() == "https" {
            let server_name = match parse_ip_literal(&validated.host)
                .map(|ip| ServerName::IpAddress(ip.into()))
                .or_else(|| ServerName::try_from(validated.host.clone()).ok())
            {
                Some(name) => name,
                None => {
                    evidence.transport.tls = TlsState::Failed;
                    evidence.failure_phase = Some(NetworkFailurePhase::Tls);
                    self.record_effect(ctx, target, evidence, None, None);
                    self.remember(unavailable_error());
                    return Err(FetchError::Unavailable);
                }
            };
            let Some(tls) = self.tls.as_ref() else {
                // validate_url 已在解析前拒绝；这里只防止无根时继续握手。
                self.remember(denied_error(NO_TLS_ROOTS));
                return Err(FetchError::Denied);
            };
            let connector = TlsConnector::from(Arc::clone(tls));
            let tls = match controlled(ctx, connector.connect(server_name, stream)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(_)) => {
                    evidence.transport.tls = TlsState::Failed;
                    evidence.failure_phase = Some(NetworkFailurePhase::Tls);
                    self.record_effect(ctx, target, evidence, None, None);
                    self.remember(unavailable_error());
                    return Err(FetchError::Unavailable);
                }
                Err(wait) => {
                    evidence.transport.tls = TlsState::Failed;
                    evidence.failure_phase = Some(NetworkFailurePhase::Tls);
                    self.record_effect(ctx, target, evidence, None, None);
                    return Err(self.wait_refusal(wait));
                }
            };
            evidence.transport.tls = TlsState::Established {
                version: match tls.get_ref().1.protocol_version() {
                    Some(rustls::ProtocolVersion::TLSv1_2) => TlsVersion::Tls12,
                    Some(rustls::ProtocolVersion::TLSv1_3) => TlsVersion::Tls13,
                    _ => TlsVersion::Other,
                },
            };
            send_request(tls, request, ctx).await
        } else {
            evidence.transport.tls = TlsState::NotApplicable;
            send_request(stream, request, ctx).await
        };

        let response = match response {
            Ok(response) => response,
            Err(wait) => {
                evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
                self.record_effect(ctx, target, evidence, None, None);
                return Err(self.wait_refusal(wait));
            }
        };
        let status = response.status();
        evidence.http_status = Some(status.as_u16());
        if status.as_u16() > 599 || !response_headers_within_limits(response.headers()) {
            evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
            self.record_effect(ctx, target, evidence, None, None);
            self.remember(limit_error("network_headers"));
            return Err(FetchError::LimitExceeded);
        }
        if response
            .headers()
            .get_all(CONTENT_ENCODING)
            .iter()
            .any(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
            self.record_effect(ctx, target, evidence, None, None);
            self.remember(unavailable_error());
            return Err(FetchError::Unavailable);
        }

        if is_redirect(status)
            && let Some(location) = response.headers().get(LOCATION)
        {
            let location = match location.to_str() {
                Ok(location) => location,
                Err(_) => {
                    evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
                    self.record_effect(ctx, target, evidence, None, None);
                    self.remember(unavailable_error());
                    return Err(FetchError::Unavailable);
                }
            };
            let next = match validated.url.join(location) {
                Ok(next) if next.as_str().len() <= MAX_URL_BYTES => next,
                _ => {
                    evidence.failure_phase = Some(NetworkFailurePhase::HttpHeaders);
                    self.record_effect(ctx, target, evidence, None, None);
                    self.remember(limit_error("redirect_url"));
                    return Err(FetchError::LimitExceeded);
                }
            };
            self.record_effect(ctx, target, evidence, None, None);
            return Ok(HopOutcome::Redirect(next.into()));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let raw = match controlled(
            ctx,
            read_body(response.into_body(), self.policy.max_web_response_bytes),
        )
        .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(())) => {
                evidence.failure_phase = Some(NetworkFailurePhase::Body);
                self.record_effect(ctx, target, evidence, None, None);
                self.remember(unavailable_error());
                return Err(FetchError::Unavailable);
            }
            Err(wait) => {
                evidence.failure_phase = Some(NetworkFailurePhase::Body);
                self.record_effect(ctx, target, evidence, None, None);
                return Err(self.wait_refusal(wait));
            }
        };
        let bytes_received = raw.bytes.len();
        evidence.body_bytes_received = Some(bytes_received as u64);
        let (body, truncated) = shape_body(raw.bytes, raw.more, self.policy.max_web_response_bytes);
        if let Some(wait) = control_state(ctx) {
            evidence.failure_phase = Some(NetworkFailurePhase::Body);
            self.record_effect(ctx, target, evidence, None, None);
            return Err(self.wait_refusal(wait));
        }
        let bytes_returned = body.len();
        let final_origin = validated.url.origin().ascii_serialization();
        let observation_bytes = bytes_returned
            .saturating_add(content_type.as_ref().map_or(0, String::len))
            .saturating_add(final_origin.len());
        if let Err(refusal) = (self.charge)(observation_bytes) {
            self.record_effect(ctx, target, evidence, None, Some(truncated));
            self.remember(budget_error(&refusal));
            return Err(FetchError::LimitExceeded);
        }
        evidence.body_bytes_returned = Some(bytes_returned as u64);
        self.record_effect(
            ctx,
            target,
            evidence,
            Some(bytes_returned as u64),
            Some(truncated),
        );

        Ok(HopOutcome::Final(FetchResponse {
            status: status.as_u16(),
            content_type,
            body,
            truncated,
            final_origin,
            redirect_count: hop,
        }))
    }

    fn record_effect(
        &self,
        ctx: &NetworkCallContext,
        target: ObservedTarget,
        evidence: NetworkEvidence,
        context_measure: Option<u64>,
        truncated: Option<bool>,
    ) {
        ctx.evidence.record_network_effect(
            self.cell(),
            self.scope(ctx),
            target,
            context_measure,
            truncated,
            evidence,
        );
    }

    fn wait_refusal(&self, wait: WaitFailure) -> FetchError {
        match wait {
            WaitFailure::Cancelled => {
                self.remember(cancelled_error(METHOD));
                FetchError::Cancelled
            }
            WaitFailure::Timeout => {
                self.remember(timeout_error());
                FetchError::Timeout
            }
            WaitFailure::Unavailable => {
                self.remember(unavailable_error());
                FetchError::Unavailable
            }
        }
    }
}

#[derive(Clone)]
struct NetworkCallContext {
    turn_id: String,
    admitted: Arc<AdmittedTool>,
    evidence: CellEvidenceLedger,
    cancellation: crate::control::Cancellation,
    deadline: Instant,
}

struct ValidatedUrl {
    url: Url,
    host: String,
    port: u16,
    localhost: bool,
}

enum HopOutcome {
    Final(FetchResponse),
    Redirect(String),
}

#[derive(Clone, Copy)]
enum WaitFailure {
    Cancelled,
    Timeout,
    Unavailable,
}

fn control_state(ctx: &NetworkCallContext) -> Option<WaitFailure> {
    if ctx.cancellation.is_cancelled() {
        Some(WaitFailure::Cancelled)
    } else if Instant::now() >= ctx.deadline {
        Some(WaitFailure::Timeout)
    } else {
        None
    }
}

async fn controlled<F, T>(ctx: &NetworkCallContext, future: F) -> Result<T, WaitFailure>
where
    F: Future<Output = T>,
{
    let mut cancellation = ctx.cancellation.clone();
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(WaitFailure::Cancelled),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(ctx.deadline)) => {
            Err(WaitFailure::Timeout)
        }
        value = future => Ok(value),
    }
}

fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

async fn resolve_candidates(
    validated: &ValidatedUrl,
    ctx: &NetworkCallContext,
) -> Result<Vec<SocketAddr>, WaitFailure> {
    if let Some(ip) = parse_ip_literal(&validated.host) {
        return Ok(vec![SocketAddr::new(ip, validated.port)]);
    }
    let resolved = controlled(
        ctx,
        tokio::net::lookup_host((validated.host.as_str(), validated.port)),
    )
    .await?;
    let mut candidates: Vec<SocketAddr> = resolved.map_err(|_| WaitFailure::Unavailable)?.collect();
    candidates.sort_unstable();
    candidates.dedup();
    if candidates.is_empty() {
        return Err(WaitFailure::Unavailable);
    }
    Ok(candidates)
}

fn candidates_allowed(validated: &ValidatedUrl, candidates: &[SocketAddr]) -> bool {
    if candidates.is_empty() {
        return false;
    }
    if validated.localhost {
        candidates
            .iter()
            .all(|candidate| candidate.ip().is_loopback())
    } else {
        candidates
            .iter()
            .all(|candidate| !net_guard::is_forbidden_tool_target_ip(candidate.ip()))
    }
}

async fn connect_candidates(
    candidates: &[SocketAddr],
    ctx: &NetworkCallContext,
) -> Result<TcpStream, WaitFailure> {
    for candidate in candidates {
        match controlled(ctx, TcpStream::connect(candidate)).await? {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(_) => continue,
        }
    }
    Err(WaitFailure::Unavailable)
}

/// 没有可用 TLS 根时 https 请求的拒绝说明，与模型请求的规则一致：https 拒绝，http 照常。
const NO_TLS_ROOTS: &str =
    "no usable TLS root certificates: https requests are refused, http requests are allowed";

/// 空集合返回 `None`：没有信任根时不构造 TLS 配置，https 请求由 Host 直接拒绝，
/// 不回退后台的系统根。
pub(crate) fn tls_config(roots_der: &[Vec<u8>]) -> Result<Option<Arc<ClientConfig>>, String> {
    if roots_der.is_empty() {
        return Ok(None);
    }
    let mut roots = rustls::RootCertStore::empty();
    for der in roots_der {
        roots
            .add(der.as_slice().into())
            .map_err(|_| "invalid Tool TLS root certificate".to_string())?;
    }
    let mut config =
        ClientConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_safe_default_protocol_versions()
            .map_err(|_| "cannot configure Tool TLS protocol versions".to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Some(Arc::new(config)))
}

async fn send_request<I>(
    io: I,
    request: Request<Empty<Bytes>>,
    ctx: &NetworkCallContext,
) -> Result<hyper::Response<Incoming>, WaitFailure>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = controlled(ctx, http1::handshake(TokioIo::new(io)))
        .await?
        .map_err(|_| WaitFailure::Unavailable)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    controlled(ctx, sender.send_request(request))
        .await?
        .map_err(|_| WaitFailure::Unavailable)
}

struct RawBody {
    bytes: Vec<u8>,
    more: bool,
}

async fn read_body(mut body: Incoming, limit: usize) -> Result<RawBody, ()> {
    let mut bytes = Vec::new();
    let read_limit = limit.saturating_add(1);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ())?;
        if let Ok(data) = frame.into_data() {
            let remaining = read_limit.saturating_sub(bytes.len());
            bytes.extend_from_slice(&data[..data.len().min(remaining)]);
            if bytes.len() == read_limit {
                return Ok(RawBody { bytes, more: true });
            }
        }
    }
    Ok(RawBody { bytes, more: false })
}

fn shape_body(raw: Vec<u8>, more: bool, limit: usize) -> (Vec<u8>, bool) {
    let lossy = String::from_utf8_lossy(&raw);
    let mut end = lossy.len().min(limit);
    while !lossy.is_char_boundary(end) {
        end -= 1;
    }
    let truncated = more || lossy.len() > end;
    (lossy.as_bytes()[..end].to_vec(), truncated)
}

fn validated_headers(
    headers: Vec<bindings::lattice::tool_host::network_fetch::Header>,
) -> Result<HeaderMap, CepError> {
    if headers.len() > MAX_REQUEST_HEADERS {
        return Err(invalid_error("headers", "too many request headers"));
    }
    let mut parsed = HeaderMap::new();
    for header in headers {
        if header.name.len() > MAX_REQUEST_HEADER_NAME_BYTES
            || header.value.len() > MAX_REQUEST_HEADER_VALUE_BYTES
        {
            return Err(invalid_error(
                "headers",
                "request header exceeds the host limit",
            ));
        }
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| invalid_error("headers", "invalid request header name"))?;
        if forbidden_request_header(&name) {
            return Err(invalid_error(
                "headers",
                "request header is controlled by the host",
            ));
        }
        let value = HeaderValue::from_str(&header.value)
            .map_err(|_| invalid_error("headers", "invalid request header value"))?;
        parsed.insert(name, value);
    }
    Ok(parsed)
}

fn request_headers(url: &Url, caller: &HeaderMap) -> Result<HeaderMap, CepError> {
    let mut headers = caller.clone();
    let authority = &url[url::Position::BeforeHost..url::Position::AfterPort];
    headers.insert(
        HOST,
        HeaderValue::from_str(authority)
            .map_err(|_| invalid_error("url", "URL authority is not a valid header"))?,
    );
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    headers.insert(CONNECTION, HeaderValue::from_static("close"));
    let bytes = headers
        .iter()
        .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
        .sum::<usize>();
    if bytes > MAX_REQUEST_HEADER_BYTES {
        return Err(invalid_error(
            "headers",
            "request headers exceed the host limit",
        ));
    }
    Ok(headers)
}

fn build_request(url: &Url, headers: HeaderMap) -> Result<Request<Empty<Bytes>>, CepError> {
    let mut path = url.path().to_string();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(path)
        .body(Empty::new())
        .map_err(|_| invalid_error("url", "URL path is not a valid HTTP target"))?;
    *request.headers_mut() = headers;
    Ok(request)
}

fn response_headers_within_limits(headers: &HeaderMap) -> bool {
    headers.len() <= MAX_RESPONSE_HEADERS
        && headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
            .sum::<usize>()
            <= MAX_RESPONSE_HEADER_BYTES
}

fn forbidden_request_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-authorization"
            | "proxy-connection"
            | "proxy-authenticate"
            | "keep-alive"
            | "te"
            | "trailer"
            | "accept-encoding"
    )
}

fn is_redirect(status: StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

fn endpoint(address: SocketAddr) -> NetworkEndpoint {
    NetworkEndpoint {
        ip: address.ip(),
        port: address.port(),
    }
}

fn evidence_url(url: &Url) -> String {
    let mut sanitized = url.clone();
    let _ = sanitized.set_username("");
    let _ = sanitized.set_password(None);
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

fn empty_network_evidence(hop: u32, url: &Url, verdict: NetworkPolicyVerdict) -> NetworkEvidence {
    NetworkEvidence {
        hop,
        url: evidence_url(url),
        resolution: NetworkResolution::NotAttempted,
        dns_candidates: Vec::new(),
        transport: TransportEvidence {
            connected_peer: None,
            tls: TlsState::NotAttempted,
        },
        http_status: None,
        body_bytes_received: None,
        body_bytes_returned: None,
        policy_verdict: verdict,
        failure_phase: None,
    }
}

fn invalid_error(field: &str, reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::TypeError,
        format!("invalid network {field}"),
        CepErrorData::Validation(ValidationErrorData {
            field: field.into(),
            reason: reason.into(),
        }),
        Retriability::Permanent,
    )
}

fn denied_error(message: &str) -> CepError {
    CepError::new(
        CepErrorCode::AmbientAuthorityDenied,
        message,
        CepErrorData::Permission(PermissionErrorData {
            required_capability: "network.fetch".into(),
            granted_capabilities: Vec::new(),
        }),
        Retriability::Permanent,
    )
}

fn effect_error(reason: String) -> CepError {
    CepError::new(
        CepErrorCode::CapabilityViolation,
        "network operation cell is not authorized",
        CepErrorData::Capability(Box::new(CapabilityErrorData {
            method: METHOD.into(),
            violations: vec![reason],
            cell_ref: None,
            capability: Some("network.fetch".into()),
        })),
        Retriability::Permanent,
    )
}

fn unavailable_error() -> CepError {
    CepError::tool_execution(
        CepErrorCode::ExecutionFailed,
        METHOD,
        "network_unavailable",
        None,
        Retriability::Transient,
    )
}

fn timeout_error() -> CepError {
    CepError::tool_execution(
        CepErrorCode::ToolTimeout,
        METHOD,
        "timeout",
        None,
        Retriability::Transient,
    )
}

fn limit_error(resource: &str) -> CepError {
    CepError::tool_execution(
        CepErrorCode::ToolResourceExhausted,
        METHOD,
        "resource_exhausted",
        Some(resource.into()),
        Retriability::Permanent,
    )
}

fn budget_error(refusal: &BudgetRefusal) -> CepError {
    CepError::new(
        CepErrorCode::Reserved,
        format!("observation budget exhausted: {}", refusal.limit),
        CepErrorData::Reserved(ReservedErrorData {
            code: refusal.limit.into(),
            detail: format!(
                "requested={} remaining={}",
                refusal.requested, refusal.remaining
            ),
        }),
        Retriability::RequiresReplan,
    )
}

impl bindings::lattice::tool_host::network_fetch::Host for StoreState {
    async fn fetch(&mut self, request: FetchRequest) -> Result<FetchResponse, FetchError> {
        let Some(host) = self.call.network.clone() else {
            return Err(FetchError::Denied);
        };
        let ctx = NetworkCallContext {
            turn_id: self.call.turn_id.clone(),
            admitted: Arc::clone(&self.call.admitted),
            evidence: self.call.evidence.clone(),
            cancellation: self.call.cancellation.clone(),
            deadline: self.deadline,
        };
        host.fetch(request, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossy_body_is_capped_after_utf8_expansion() {
        let (body, truncated) = shape_body(vec![0xff, b'a'], false, 3);
        assert_eq!(body, "�".as_bytes());
        assert!(truncated);
    }

    #[test]
    fn host_controlled_headers_are_rejected() {
        let error = validated_headers(vec![bindings::lattice::tool_host::network_fetch::Header {
            name: "Host".into(),
            value: "evil.example".into(),
        }])
        .expect_err("Host must stay host-owned");
        assert_eq!(error.code, CepErrorCode::TypeError);
    }

    #[test]
    fn evidence_url_omits_credentials_query_and_fragment() {
        let url = Url::parse("https://user:pass@example.com/path?token=secret#fragment").unwrap();
        assert_eq!(evidence_url(&url), "https://example.com/path");
    }

    #[test]
    fn dns_candidates_fail_closed_on_empty_private_or_mixed_answers() {
        let public = ValidatedUrl {
            url: Url::parse("https://example.com/").unwrap(),
            host: "example.com".into(),
            port: 443,
            localhost: false,
        };
        let public_addr = "8.8.8.8:443".parse().unwrap();
        let private_addr = "127.0.0.1:443".parse().unwrap();
        assert!(candidates_allowed(&public, &[public_addr]));
        assert!(!candidates_allowed(&public, &[]));
        assert!(!candidates_allowed(&public, &[private_addr]));
        assert!(!candidates_allowed(&public, &[public_addr, private_addr]));

        let localhost = ValidatedUrl {
            url: Url::parse("http://localhost/").unwrap(),
            host: "localhost".into(),
            port: 80,
            localhost: true,
        };
        assert!(candidates_allowed(
            &localhost,
            &["127.0.0.1:80".parse().unwrap()]
        ));
        assert!(!candidates_allowed(
            &localhost,
            &[
                "127.0.0.1:80".parse().unwrap(),
                "8.8.8.8:80".parse().unwrap()
            ]
        ));
    }
}

#[cfg(test)]
#[tokio::test]
async fn budget_refusals_distinguish_pre_and_post_io() {
    use std::collections::BTreeSet;
    use std::time::Duration;

    use lancet_protocol::cell::Trust;
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nbody")
            .await
            .unwrap();
    });

    let runtime = crate::ComponentRuntime::from_embedded().unwrap();
    let descriptor = runtime.descriptor(METHOD).unwrap();
    let admitted = crate::admission::admit_tool(
        descriptor,
        &CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from([METHOD.to_string()]),
            denied_capabilities: BTreeSet::new(),
            max_boundary: Trust::Ex,
        },
    )
    .unwrap();
    let evidence = CellEvidenceLedger::new();
    evidence.set_admission(admitted.evidence.clone());
    let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let ctx = NetworkCallContext {
        turn_id: "test-turn".into(),
        admitted: Arc::new(admitted),
        evidence: evidence.clone(),
        cancellation: crate::control::Cancellation::new(cancel_rx),
        deadline: Instant::now() + Duration::from_secs(5),
    };
    let mut policy = NetworkPolicyConfig::safe_defaults();
    policy.allow_localhost = true;
    policy.allowed_hosts = vec!["127.0.0.1".into()];

    let preflight_evidence = CellEvidenceLedger::new();
    preflight_evidence.set_admission(ctx.admitted.evidence.clone());
    let preflight_ctx = NetworkCallContext {
        evidence: preflight_evidence.clone(),
        ..ctx.clone()
    };
    let preflight_host = NetworkFetchHost::new(
        policy.clone(),
        Arc::new(|_| panic!("preflight refusal must not charge")),
        Arc::new(|| 0),
        "external-host",
        tls_config(&[]).unwrap(),
    );
    let preflight_error = preflight_host
        .fetch(
            FetchRequest {
                url: format!("http://{addr}/"),
                headers: Vec::new(),
            },
            preflight_ctx,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(preflight_error, FetchError::LimitExceeded),
        "{preflight_error:?}"
    );
    let (_, effects, denied) = preflight_evidence.snapshot();
    assert!(effects.effects.is_empty());
    assert_eq!(denied.denied.len(), 1);
    let network = denied.denied[0].network.as_ref().unwrap();
    assert_eq!(network.resolution, NetworkResolution::NotAttempted);
    assert!(network.failure_phase.is_none());

    let host = NetworkFetchHost::new(
        policy,
        Arc::new(|requested| {
            Err(BudgetRefusal {
                limit: "observation.max_turn_observation_bytes",
                requested,
                remaining: 0,
            })
        }),
        Arc::new(|| usize::MAX),
        "external-host",
        tls_config(&[]).unwrap(),
    );

    let error = host
        .fetch(
            FetchRequest {
                url: format!("http://{addr}/"),
                headers: Vec::new(),
            },
            ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, FetchError::LimitExceeded), "{error:?}");
    let (_, effects, denied) = evidence.snapshot();
    assert_eq!(effects.effects.len(), 1);
    assert!(denied.denied.is_empty());
    assert_eq!(effects.effects[0].context_measure, None);
    let network = effects.effects[0].network.as_ref().unwrap();
    assert_eq!(network.http_status, Some(200));
    assert_eq!(network.body_bytes_received, Some(4));
    assert_eq!(network.body_bytes_returned, None);
}

/// LAN-17：一次 Tool 调用只能产生一条受限请求链。逐条对应合同列出的失败方式：
/// 公网 `http://` 仍被连接；同一次调用成功发起第二次 network import；
/// Host 跟随第六次重定向；每跳重新获得完整 deadline，使整条链超过调用预算。
#[cfg(test)]
mod request_chain {
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use lancet_protocol::cell::Trust;
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn call_ctx(deadline: Duration) -> (NetworkCallContext, tokio::sync::watch::Sender<bool>) {
        let runtime = crate::ComponentRuntime::from_embedded().unwrap();
        let admitted = crate::admission::admit_tool(
            runtime.descriptor(METHOD).unwrap(),
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from([METHOD.to_string()]),
                denied_capabilities: BTreeSet::new(),
                max_boundary: Trust::Ex,
            },
        )
        .unwrap();
        let evidence = CellEvidenceLedger::new();
        evidence.set_admission(admitted.evidence.clone());
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let ctx = NetworkCallContext {
            turn_id: "test-turn".into(),
            admitted: Arc::new(admitted),
            evidence,
            cancellation: crate::control::Cancellation::new(cancel_rx),
            deadline: Instant::now() + deadline,
        };
        (ctx, cancel_tx)
    }

    fn host(allowed_hosts: &[&str], allow_public_https: bool) -> NetworkFetchHost {
        let mut policy = NetworkPolicyConfig::safe_defaults();
        policy.allow_localhost = true;
        policy.allow_public_https = allow_public_https;
        policy.allowed_hosts = allowed_hosts.iter().map(|h| h.to_string()).collect();
        NetworkFetchHost::new(
            policy,
            Arc::new(|_| Ok(())),
            Arc::new(|| usize::MAX),
            "external-host",
            tls_config(&[]).unwrap(),
        )
    }

    fn get(url: String) -> FetchRequest {
        FetchRequest {
            url,
            headers: Vec::new(),
        }
    }

    /// `/r/N` 在等待 `delay` 后重定向到 `/r/N-1`，`/r/0` 返回 200；返回收到的请求数。
    async fn redirect_server(delay: Duration) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let counter = Arc::clone(&counter);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let left: u32 = String::from_utf8_lossy(&request)
                        .split_whitespace()
                        .nth(1)
                        .and_then(|path| path.strip_prefix("/r/")?.parse().ok())
                        .unwrap_or(0);
                    tokio::time::sleep(delay).await;
                    let response = if left > 0 {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: /r/{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            left - 1
                        )
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into()
                    };
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (addr, hits)
    }

    /// 只接受连接、不应答的监听器；每接受一个连接就送出对端地址。
    async fn connection_listener() -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<SocketAddr>)
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (peers_tx, peers) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            // 保持连接打开，避免对端在握手中途看到关闭。
            let mut held = Vec::new();
            while let Ok((stream, peer)) = listener.accept().await {
                held.push(stream);
                if peers_tx.send(peer).is_err() {
                    return;
                }
            }
        });
        (addr, peers)
    }

    /// 被测请求返回后再连一次探针：内核按到达顺序交出已完成握手的连接，
    /// 所以探针之前被接受的连接数就是被测代码对该监听器发起的连接数；
    /// 不依赖接受任务恰好已经运行。
    async fn connections_before_probe(
        addr: SocketAddr,
        mut peers: tokio::sync::mpsc::UnboundedReceiver<SocketAddr>,
    ) -> usize {
        let probe = tokio::net::TcpStream::connect(addr).await.unwrap();
        let probe_addr = probe.local_addr().unwrap();
        let mut before = 0;
        loop {
            let peer = tokio::time::timeout(Duration::from_secs(5), peers.recv())
                .await
                .expect("probe connection was accepted")
                .expect("listener is still running");
            if peer == probe_addr {
                return before;
            }
            before += 1;
        }
    }

    #[tokio::test]
    async fn public_http_is_refused_and_a_call_gets_one_import() {
        let (ctx, _cancel) = call_ctx(Duration::from_secs(5));
        // 两个目标都在允许列表内，第二次被拒绝只能来自“一次调用一个 import”。
        let host = host(&["example.com", "127.0.0.1"], true);

        let first = host
            .fetch(get("http://example.com/".into()), ctx.clone())
            .await
            .unwrap_err();
        assert!(matches!(first, FetchError::Denied), "{first:?}");
        let (_, effects, denied) = ctx.evidence.snapshot();
        assert!(effects.effects.is_empty(), "public http must not connect");
        let network = denied.denied[0].network.as_ref().unwrap();
        assert_eq!(network.resolution, NetworkResolution::NotAttempted);

        let (addr, hits) = redirect_server(Duration::ZERO).await;
        let second = host
            .fetch(get(format!("http://{addr}/r/0")), ctx)
            .await
            .unwrap_err();
        assert!(matches!(second, FetchError::Denied), "{second:?}");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "second import reached the network"
        );
    }

    /// 没有可用 TLS 根时，Tool Host 与模型请求同一规则：https 在解析与连接前拒绝，
    /// 不回退系统根；http 照常放行。旧实现会先连上再在握手失败（Unavailable）。
    #[tokio::test]
    async fn without_tls_roots_https_is_refused_before_connect_and_http_is_allowed() {
        let (tls_addr, peers) = connection_listener().await;
        assert!(
            tls_config(&[]).unwrap().is_none(),
            "empty roots build no TLS config"
        );

        let (ctx, _cancel) = call_ctx(Duration::from_secs(5));
        let https_host = host(&["127.0.0.1"], false);
        let error = https_host
            .fetch(get(format!("https://{tls_addr}/")), ctx.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(error, FetchError::Denied),
            "https error kind: {error:?}"
        );
        let refusal = format!("{:?}", https_host.take_refusal());
        assert!(
            refusal.contains("no usable TLS root certificates"),
            "refusal must name the missing roots: {refusal}"
        );
        assert_eq!(
            connections_before_probe(tls_addr, peers).await,
            0,
            "https without roots must not open a connection"
        );
        let (_, effects, denied) = ctx.evidence.snapshot();
        assert!(
            effects.effects.is_empty(),
            "https without roots recorded an effect"
        );
        assert_eq!(denied.denied.len(), 1, "one denial for the https request");
        assert_eq!(denied.denied[0].reason, "tls_roots_unavailable");
        let network = denied.denied[0].network.as_ref().unwrap();
        assert_eq!(network.resolution, NetworkResolution::NotAttempted);

        let (http_addr, hits) = redirect_server(Duration::ZERO).await;
        let (ctx, _cancel) = call_ctx(Duration::from_secs(5));
        let response = host(&["127.0.0.1"], false)
            .fetch(get(format!("http://{http_addr}/r/0")), ctx)
            .await
            .expect("http without roots is allowed");
        assert_eq!(response.redirect_count, 0);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "http request reached the server"
        );
    }

    /// 没有可用 TLS 根时，策略或单元授权不允许的 https 目标仍按原因报告拒绝，
    /// 只有本来会被放行的目标才报告 `tls_roots_unavailable`；三种情况都不解析、不连接。
    #[tokio::test]
    async fn without_tls_roots_policy_denials_take_precedence_over_missing_roots() {
        let (addr, peers) = connection_listener().await;
        let not_allowlisted = host(&[], false);
        let mut not_admitted = host(&["127.0.0.1"], false);
        not_admitted.scope_name = "unadmitted-scope".into();
        let allowed = host(&["127.0.0.1"], false);
        for (name, fetch_host, url, reason) in [
            (
                "https target outside the allowlist",
                &not_allowlisted,
                "https://192.0.2.1/".to_string(),
                "network_policy",
            ),
            (
                "https target in a scope the tool was not admitted for",
                &not_admitted,
                format!("https://{addr}/"),
                "cell_denied",
            ),
            (
                "allowlisted and admitted https target",
                &allowed,
                format!("https://{addr}/"),
                "tls_roots_unavailable",
            ),
        ] {
            let (ctx, _cancel) = call_ctx(Duration::from_secs(5));
            let error = fetch_host.fetch(get(url), ctx.clone()).await.unwrap_err();
            assert!(
                matches!(error, FetchError::Denied),
                "{name}: error kind {error:?}"
            );
            let (_, effects, denied) = ctx.evidence.snapshot();
            assert!(effects.effects.is_empty(), "{name}: recorded an effect");
            assert_eq!(denied.denied.len(), 1, "{name}: one denial");
            assert_eq!(denied.denied[0].reason, reason, "{name}: denial reason");
            let network = denied.denied[0].network.as_ref().unwrap();
            assert_eq!(
                network.resolution,
                NetworkResolution::NotAttempted,
                "{name}: no resolution"
            );
            let refusal = format!("{:?}", fetch_host.take_refusal());
            assert_eq!(
                refusal.contains("no usable TLS root certificates"),
                reason == "tls_roots_unavailable",
                "{name}: only the allowed target names the missing roots: {refusal}"
            );
        }
        assert_eq!(
            connections_before_probe(addr, peers).await,
            0,
            "no https request without roots opens a connection"
        );
    }

    /// 没有可用 TLS 根时，允许且已授权的 http 目标重定向到 https，
    /// 在重定向那一跳（hop 1）按 `tls_roots_unavailable` 拒绝：http 第一跳照常发出，
    /// https 目标零连接。只在第一跳检查的实现会去连接 https 目标而失败。
    #[tokio::test]
    async fn without_tls_roots_an_http_redirect_to_https_is_refused_at_the_redirect_hop() {
        let (tls_addr, tls_peers) = connection_listener().await;

        // http 一侧对任何请求都以绝对 https Location 重定向到上面的监听器。
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let http_counter = Arc::clone(&hits);
        let location = format!("https://{tls_addr}/target");
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = http_listener.accept().await {
                let counter = Arc::clone(&http_counter);
                let location = location.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        let (ctx, _cancel) = call_ctx(Duration::from_secs(5));
        // 两个目标同为 127.0.0.1，都在允许列表内且单元已授权，拒绝只能来自缺少根证书。
        let fetch_host = host(&["127.0.0.1"], false);
        let error = fetch_host
            .fetch(get(format!("http://{http_addr}/start")), ctx.clone())
            .await
            .unwrap_err();
        assert!(
            matches!(error, FetchError::Denied),
            "redirect to https error kind: {error:?}"
        );
        let refusal = format!("{:?}", fetch_host.take_refusal());
        assert!(
            refusal.contains("no usable TLS root certificates"),
            "refusal must name the missing roots: {refusal}"
        );
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the http first hop reached the server"
        );
        assert_eq!(
            connections_before_probe(tls_addr, tls_peers).await,
            0,
            "the https redirect target must not get a connection"
        );
        let (_, effects, denied) = ctx.evidence.snapshot();
        assert_eq!(
            effects.effects.len(),
            1,
            "only the http hop recorded an effect"
        );
        assert_eq!(denied.denied.len(), 1, "one denial for the redirect hop");
        assert_eq!(
            denied.denied[0].reason, "tls_roots_unavailable",
            "redirect hop denial reason"
        );
        let network = denied.denied[0].network.as_ref().unwrap();
        assert_eq!(network.hop, 1, "denied at the redirect hop");
        assert_eq!(
            network.resolution,
            NetworkResolution::NotAttempted,
            "redirect target was not resolved"
        );
        assert!(
            network.url.starts_with("https://"),
            "denied hop is the https target: {}",
            network.url
        );
    }

    #[tokio::test]
    async fn redirect_chain_follows_five_hops_under_one_deadline() {
        enum Expect {
            Redirects(u32),
            Limit,
            Timeout,
        }
        // (重定向次数, 每跳延迟, 调用 deadline, 期望结果, 服务端最多收到的请求数)
        let cases = [
            (
                5,
                Duration::ZERO,
                Duration::from_secs(5),
                Expect::Redirects(5),
                6,
            ),
            (6, Duration::ZERO, Duration::from_secs(5), Expect::Limit, 6),
            // 每跳都短于 deadline；只有整条链共用 deadline 才会超时。
            (
                5,
                Duration::from_millis(300),
                Duration::from_secs(1),
                Expect::Timeout,
                4,
            ),
        ];
        for (redirects, delay, deadline, expect, max_requests) in cases {
            let (addr, hits) = redirect_server(delay).await;
            let (ctx, _cancel) = call_ctx(deadline);
            let result = host(&["127.0.0.1"], false)
                .fetch(get(format!("http://{addr}/r/{redirects}")), ctx)
                .await;
            let matched = match (&result, &expect) {
                (Ok(response), Expect::Redirects(n)) => response.redirect_count == *n,
                (Err(FetchError::LimitExceeded), Expect::Limit) => true,
                (Err(FetchError::Timeout), Expect::Timeout) => true,
                _ => false,
            };
            assert!(
                matched,
                "{redirects} redirects, {delay:?} per hop: {result:?}"
            );
            let requests = hits.load(Ordering::SeqCst);
            assert!(
                requests <= max_requests,
                "{redirects} redirects: server saw {requests} requests"
            );
        }
    }
}
