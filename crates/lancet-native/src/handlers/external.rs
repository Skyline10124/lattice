// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP `fetch.web` CEP handler.

use std::sync::OnceLock;

use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::executor::web::fetch_url;
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::policy::budget::BudgetExhausted;
use lancet_authority::policy::network::validate_fetch_url;
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::error::ExecutionErrorData;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use reqwest::redirect::Policy;
use serde_json::json;

fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("native fetch.web http client")
    })
}

/// Register native `fetch.web`.
pub fn register_external_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Fetch, _, _>(handle_fetch)?;
    Ok(())
}

async fn handle_fetch(params: FetchParams, ctx: HandlerContext) -> Result<FetchResult, CepError> {
    handle_fetch_as(params, ctx, Fetch::NAME).await
}

async fn handle_fetch_as(
    params: FetchParams,
    ctx: HandlerContext,
    method: &'static str,
) -> Result<FetchResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, &params).await?;

    let policy = &ctx.cep_context.policy.network;
    let validated = match validate_fetch_url(&params.url, policy) {
        Ok(v) => v,
        Err(err) => {
            emit_denial_metadata(&ctx, &params.url, false, method).await;
            return Err(err);
        }
    };

    let max_bytes = policy.max_web_response_bytes;
    let pin_public = !validated.use_localhost_client;
    let outcome = match fetch_url(
        shared_http_client(),
        validated.url.as_str(),
        &params.headers,
        max_bytes,
        pin_public,
    )
    .await
    {
        Ok(o) => o,
        Err(message) => {
            emit_denial_metadata(&ctx, &params.url, true, method).await;
            return Err(http_execution_error(message));
        }
    };

    let bytes_returned = outcome.bytes_returned;
    if let Err(budget_err) = ctx
        .cep_context
        .budget
        .try_charge_observation(bytes_returned)
    {
        emit_budget_denial_metadata(&ctx, &params.url, &budget_err, method).await;
        return Err(budget_err.to_cep_error());
    }

    let host = validated.url.host_str().unwrap_or("unknown").to_string();
    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::ExternalObservation,
        scope: ObservedScope::Host(host),
        required_capability: RequiredCapability::FetchUrl,
        source: EffectSource::HandlerBackend,
    });

    emit_success_metadata(
        &ctx,
        &params.url,
        outcome.response_size,
        bytes_returned,
        outcome.response_size_known,
        outcome.truncated,
        method,
    )
    .await;

    Ok(FetchResult {
        status: outcome.status,
        body: outcome.body,
        content_type: outcome.content_type,
        truncated: outcome.truncated.then_some(true),
    })
}

fn http_execution_error(message: String) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        message.clone(),
        CepErrorData::Execution(ExecutionErrorData {
            command: Fetch::NAME.into(),
            exit_code: None,
            stderr: Some(message),
        }),
        Retriability::Transient,
    )
}

async fn emit_success_metadata(
    ctx: &HandlerContext,
    request_url: &str,
    response_size: usize,
    bytes_returned: usize,
    response_size_known: bool,
    truncated: bool,
    method: &'static str,
) {
    let metadata = json!({
        "method": method,
        "request_url": request_url,
        "observation_source": "external",
        "response_size": response_size,
        "bytes_returned": bytes_returned,
        "response_size_known": response_size_known,
        "truncated": truncated,
        "policy_enforced": true,
        "budget_charged": bytes_returned,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

async fn emit_denial_metadata(
    ctx: &HandlerContext,
    request_url: &str,
    network_attempted: bool,
    method: &'static str,
) {
    let metadata = json!({
        "method": method,
        "request_url": request_url,
        "denial_source": if network_attempted { "network" } else { "cep_policy" },
        "policy_enforced": true,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

async fn emit_budget_denial_metadata(
    ctx: &HandlerContext,
    request_url: &str,
    budget_err: &BudgetExhausted,
    method: &'static str,
) {
    let metadata = json!({
        "method": method,
        "request_url": request_url,
        "denial_source": "cep_budget",
        "policy_enforced": true,
        "budget_limit": budget_err.limit,
        "budget_requested": budget_err.requested,
        "budget_remaining": budget_err.remaining,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, watch};

    fn test_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_external_methods(&mut registry).expect("register fetch.web");
        registry
    }

    fn ctx_for(
        cep_context: Arc<CepTurnContext>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let policy_cfg = lancet_authority::policy::default::PolicyConfig {
            capability_sources: Default::default(),
            methods: Default::default(),
            network: cep_context.policy.network.clone(),
            budget: cep_context.policy.budget.clone(),
            observation: cep_context.policy.observation.clone(),
            workspace: Default::default(),
            project_root: cep_context.policy.project_root.clone(),
            denied_effect_kinds: Default::default(),
        };
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_cfg,
        ));
        (
            HandlerContext {
                call_id: 7,
                events: EventSink::new(event_tx, 7),
                cancellation: Cancellation::new(cancel_rx),
                cep_context: Arc::clone(&cep_context),
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                cep_mode: lancet_protocol::policy::CepMode::Off,
                workspace: Arc::new(
                    crate::workspace::LocalWorkspace::new(&cep_context.policy.project_root)
                        .expect("local workspace"),
                ),
            },
            event_rx,
        )
    }

    async fn spawn_echo_server(status_line: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind localhost");
        let addr = listener.local_addr().expect("local addr");
        let body_owned = body.to_string();
        let status_owned = status_line.to_string();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status_owned}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_owned}",
                    body_owned.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        format!("http://127.0.0.1:{}/", addr.port())
    }

    #[tokio::test]
    async fn native_web_fetch_returns_real_status_not_hardcoded_200() {
        let url = spawn_echo_server("404 Not Found", "missing").await;
        let mut policy = lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, _events) = ctx_for(cep_context);

        let result = registry
            .dispatch(
                Fetch::NAME,
                serde_json::to_value(FetchParams {
                    url: url.clone(),
                    headers: Default::default(),
                })
                .unwrap(),
                ctx,
            )
            .await
            .unwrap();
        let parsed: FetchResult = serde_json::from_value(result).unwrap();
        assert_eq!(
            parsed.status, 404,
            "must reflect server status, not hardcoded 200"
        );
        assert!(parsed.body.contains("missing"));
    }

    #[tokio::test]
    async fn native_web_fetch_truncates_over_cap_response() {
        let big_body = "x".repeat(500);
        let url = spawn_echo_server("200 OK", &big_body).await;
        let mut policy = lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        policy.network.max_web_response_bytes = 100;
        policy.budget.max_turn_observation_bytes = 10_000;
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, mut events) = ctx_for(cep_context);

        let result = registry
            .dispatch(
                Fetch::NAME,
                serde_json::to_value(FetchParams {
                    url,
                    headers: Default::default(),
                })
                .unwrap(),
                ctx,
            )
            .await
            .unwrap();
        let parsed: FetchResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.body.len(), 100);
        assert_eq!(parsed.truncated, Some(true));

        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.native_external_metadata");
        assert_eq!(event.params["metadata"]["truncated"], true);
        assert_eq!(event.params["metadata"]["observation_source"], "external");
        assert!(
            !event.params["metadata"]
                .as_object()
                .unwrap()
                .contains_key("body")
        );
    }

    #[tokio::test]
    async fn native_web_fetch_denies_localhost_by_default() {
        let registry = test_registry();
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        ));
        let (ctx, mut events) = ctx_for(cep_context);

        let err = registry
            .dispatch(
                Fetch::NAME,
                serde_json::to_value(FetchParams {
                    url: "http://127.0.0.1:9/".into(),
                    headers: Default::default(),
                })
                .unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::AmbientAuthorityDenied);
        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");
        let event = events.recv().await.unwrap();
        assert_eq!(event.params["metadata"]["denial_source"], "cep_policy");
        assert!(event.params["metadata"].get("backend_invoked").is_none());
    }

    #[tokio::test]
    async fn native_web_fetch_turn_observation_budget_error() {
        let url = spawn_echo_server("200 OK", "payload").await;
        let mut policy = lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        policy.budget.max_turn_observation_bytes = 4;
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, _events) = ctx_for(cep_context);

        let err = registry
            .dispatch(
                Fetch::NAME,
                serde_json::to_value(FetchParams {
                    url,
                    headers: Default::default(),
                })
                .unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
    }
}
