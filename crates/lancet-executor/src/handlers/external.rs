//! Native Phase 21 `web.fetch` CEP handler.

use std::sync::OnceLock;

use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::executor::web::fetch_url;
use crate::guard::evaluate_and_enforce_policy;
use crate::policy::budget::BudgetExhausted;
use crate::policy::network::validate_fetch_url;
use crate::registry::{HandlerContext, MethodRegistry};
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::ExecutionErrorData;
use lancet_protocol::method::RequiredCapability;
use lancet_protocol::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, CepMethod, Retriability};
use reqwest::redirect::Policy;
use serde_json::json;

fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("native web.fetch http client")
    })
}

/// Register native `web.fetch`.
pub fn register_external_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Fetch, _, _>(handle_web_fetch)
}

async fn handle_web_fetch(
    params: FetchParams,
    ctx: HandlerContext,
) -> Result<FetchResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, Fetch::NAME, &params).await?;

    let policy = &ctx.phase21.policy.network;
    let validated = match validate_fetch_url(&params.url, policy) {
        Ok(v) => v,
        Err(err) => {
            emit_denial_metadata(&ctx, &params.url, false).await;
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
            emit_denial_metadata(&ctx, &params.url, true).await;
            return Err(http_execution_error(message));
        }
    };

    let bytes_returned = outcome.bytes_returned;
    if let Err(budget_err) = ctx.phase21.budget.try_charge_observation(bytes_returned) {
        emit_budget_denial_metadata(&ctx, &params.url, &budget_err).await;
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
            command: "web.fetch".into(),
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
) {
    let metadata = json!({
        "method": Fetch::NAME,
        "request_url": request_url,
        "observation_source": "external",
        "response_size": response_size,
        "bytes_returned": bytes_returned,
        "response_size_known": response_size_known,
        "truncated": truncated,
        "policy_enforced": true,
        "backend_invoked": true,
        "budget_charged": bytes_returned,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": Fetch::NAME, "metadata": metadata }),
        )
        .await;
}

async fn emit_denial_metadata(ctx: &HandlerContext, request_url: &str, backend_invoked: bool) {
    let metadata = json!({
        "method": Fetch::NAME,
        "request_url": request_url,
        "denial_source": if backend_invoked { "backend" } else { "cep_policy" },
        "policy_enforced": true,
        "backend_invoked": backend_invoked,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": Fetch::NAME, "metadata": metadata }),
        )
        .await;
}

async fn emit_budget_denial_metadata(
    ctx: &HandlerContext,
    request_url: &str,
    budget_err: &BudgetExhausted,
) {
    let metadata = json!({
        "method": Fetch::NAME,
        "request_url": request_url,
        "denial_source": "cep_budget",
        "policy_enforced": true,
        "backend_invoked": true,
        "budget_limit": budget_err.limit,
        "budget_requested": budget_err.requested,
        "budget_remaining": budget_err.remaining,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_external_metadata",
            json!({ "method": Fetch::NAME, "metadata": metadata }),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Phase21TurnContext;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, watch};

    fn test_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_external_methods(&mut registry).expect("register web.fetch");
        registry
    }

    fn ctx_for(
        phase21: Arc<Phase21TurnContext>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let policy_cfg = crate::policy::default::PolicyConfig {
            capability_sources: Default::default(),
            methods: Default::default(),
            command: phase21.policy.command.clone(),
            network: phase21.policy.network.clone(),
            budget: phase21.policy.budget.clone(),
            observation: phase21.policy.observation.clone(),
            project_root: phase21.policy.project_root.clone(),
        };
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(policy_cfg));
        (
            HandlerContext {
                call_id: 7,
                events: EventSink::new(event_tx, 7),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(".", "web.fetch test")
                    .with_fetch_host("*"),
                phase21,
                active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                })),
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
        let mut policy = crate::policy::Phase21PolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        let phase21 = Arc::new(Phase21TurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, _events) = ctx_for(phase21);

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
        let mut policy = crate::policy::Phase21PolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        policy.network.max_web_response_bytes = 100;
        policy.budget.max_turn_observation_bytes = 10_000;
        let phase21 = Arc::new(Phase21TurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, mut events) = ctx_for(phase21);

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
        assert!(!event.params["metadata"]
            .as_object()
            .unwrap()
            .contains_key("body"));
    }

    #[tokio::test]
    async fn native_web_fetch_denies_localhost_by_default() {
        let registry = test_registry();
        let phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        ));
        let (ctx, mut events) = ctx_for(phase21);

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
        assert_eq!(event.params["metadata"]["backend_invoked"], false);
    }

    #[tokio::test]
    async fn native_web_fetch_turn_observation_budget_error() {
        let url = spawn_echo_server("200 OK", "payload").await;
        let mut policy = crate::policy::Phase21PolicyConfig::defaults_for_project_root(
            TempDir::new().unwrap().path(),
        );
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        policy.budget.max_turn_observation_bytes = 4;
        let phase21 = Arc::new(Phase21TurnContext::from_policy(policy));
        let registry = test_registry();
        let (ctx, _events) = ctx_for(phase21);

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
