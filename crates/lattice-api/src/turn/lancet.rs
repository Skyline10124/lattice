//! Turn-scoped Lancet runtime composition and the CEP client-backed executor adapter.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use lancet_executor::{
    impact_kind_for_method, impact_kind_label, register_phase20_legacy_backend,
    to_tool_definitions, wire_code_for_error, CepAuditContext, CepAuditStore, LancetClient,
    LancetTransport, MethodRegistry, ServerRunConfig,
};
use lancet_protocol::catalog;
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::error::{
    invalid_params, jsonrpc, method_not_found, parse_error, CepErrorData,
};
use lancet_protocol::methods::command::Run;
use lancet_protocol::methods::envelope::Declare;
use lancet_protocol::methods::external::Fetch;
use lancet_protocol::methods::session::TodoWrite;
use lancet_protocol::methods::workspace::{
    FindFiles, Grep, ListDirectory, Patch, ReadFile, WriteFile,
};
use lancet_protocol::{CatalogMethod, CepError, CepMethod, Retriability, ToolEvent};
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::ToolCall;
use lattice_tooling::resolve_enabled_tools;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::session::ApiError;

/// Per-build construction metrics (test-only; each production build is exactly one pair + adapter).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Phase20BuildMetrics {
    pub adapter_instances: usize,
    pub transport_pairs: usize,
}

/// Client-backed adapter that maps LLM-shape tool calls into typed CEP invocations.
pub struct LancetExecutorAdapter {
    client: LancetClient,
    catalog_names: HashSet<String>,
    enabled_names: HashSet<String>,
}

impl LancetExecutorAdapter {
    pub fn new(
        client: LancetClient,
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let catalog_names = catalog().iter().map(|d| d.name.to_string()).collect();
        let enabled_names = enabled_names.into_iter().map(Into::into).collect();
        Self {
            client,
            catalog_names,
            enabled_names,
        }
    }

    fn map_cep_error(cep: CepError) -> ToolExecutionError {
        ToolExecutionError {
            message: cep.message.clone(),
            cep_error: Some(lattice_agent::CepErrorPayload {
                code: adapter_wire_code(&cep),
                message: cep.message,
                data: serde_json::to_value(&cep.data).unwrap_or(Value::Null),
                retriability: retriability_label(&cep.retriability),
            }),
        }
    }

    fn catalog_method_names(&self) -> &HashSet<String> {
        &self.catalog_names
    }

    async fn dispatch_typed(&self, method: &str, params: Value) -> Result<Value, CepError> {
        match method {
            ReadFile::NAME => self.call_typed::<ReadFile>(params).await,
            Grep::NAME => self.call_typed::<Grep>(params).await,
            ListDirectory::NAME => self.call_typed::<ListDirectory>(params).await,
            FindFiles::NAME => self.call_typed::<FindFiles>(params).await,
            Fetch::NAME => self.call_typed::<Fetch>(params).await,
            TodoWrite::NAME => self.call_typed::<TodoWrite>(params).await,
            Run::NAME => self.call_typed::<Run>(params).await,
            WriteFile::NAME => self.call_typed::<WriteFile>(params).await,
            Patch::NAME => self.call_typed::<Patch>(params).await,
            Declare::NAME => self.call_typed::<Declare>(params).await,
            other => Err(method_not_found(other)),
        }
    }

    async fn call_typed<M>(&self, params: Value) -> Result<Value, CepError>
    where
        M: CatalogMethod,
        M::Params: DeserializeOwned + Serialize,
        M::Result: Serialize,
    {
        let typed = serde_json::from_value::<M::Params>(params)
            .map_err(|err| schema_mismatch_for_method::<M>(err))?;
        let result = self.client.call::<M>(typed).await?;
        serde_json::to_value(result).map_err(|err| {
            CepError::internal_error(format!("failed to serialize {} result: {err}", M::NAME))
        })
    }
}

#[async_trait]
impl ToolCallExecutor for LancetExecutorAdapter {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        let params: Value = match serde_json::from_str(&call.function.arguments) {
            Ok(value) => value,
            Err(err) => {
                return Err(Self::map_cep_error(parse_error(format!(
                    "invalid tool arguments JSON: {err}"
                ))));
            }
        };

        if !self.catalog_method_names().contains(&call.function.name) {
            return Err(Self::map_cep_error(method_not_found(&call.function.name)));
        }

        if !self.enabled_names.contains(&call.function.name) {
            return Err(Self::map_cep_error(method_not_found(&call.function.name)));
        }

        match self.dispatch_typed(&call.function.name, params).await {
            Ok(value) => serde_json::to_string(&value).map_err(|err| {
                ToolExecutionError::simple(format!("failed to serialize tool result: {err}"))
            }),
            Err(cep) => Err(Self::map_cep_error(cep)),
        }
    }
}

/// Turn-scoped Lancet server lifecycle; dropped when the turn task completes.
pub(crate) struct LancetTurnRuntime {
    server_task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl LancetTurnRuntime {
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown.send(true);
        let task = std::mem::replace(&mut self.server_task, tokio::spawn(async {}));
        let _ = task.await;
    }

    #[cfg(test)]
    pub(crate) fn dummy() -> Self {
        Self {
            server_task: tokio::spawn(async {}),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }
}

impl Drop for LancetTurnRuntime {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Resolved production turn dependencies assembled by the API layer.
pub(crate) struct ProductionTurnDeps {
    pub agent_loop: Arc<AgentLoop>,
    pub tool_definitions: Vec<lattice_llm::ToolDefinition>,
    pub runtime: LancetTurnRuntime,
    pub lancet_client: LancetClient,
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    #[cfg(test)]
    pub build_metrics: Phase20BuildMetrics,
}

/// Build catalog-derived tool definitions filtered by the profile enabled list.
pub(crate) fn resolve_catalog_tool_definitions(
    enabled: &[String],
) -> Result<Vec<lattice_llm::ToolDefinition>, ApiError> {
    let inventory = to_tool_definitions(catalog());
    let resolved = resolve_enabled_tools(enabled, &inventory).map_err(|err| {
        ApiError::BadRequest(format!(
            "unknown enabled tools {:?}; available catalog tools: {:?}",
            err.unknown, err.available
        ))
    })?;
    Ok(resolved.definitions)
}

/// Construct one turn-scoped Lancet runtime and inject it into a fresh AgentLoop.
pub(crate) fn build_production_turn_deps(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn lattice_agent::LlmClient>,
    in_memory_audit: bool,
) -> Result<ProductionTurnDeps, ApiError> {
    let tool_definitions = resolve_catalog_tool_definitions(enabled_tools)?;

    let mut base_envelope =
        BaseExecutionEnvelope::for_workspace_root(project_root, "phase20 turn observation");
    if enabled_tools.iter().any(|name| name == TodoWrite::NAME) {
        base_envelope = base_envelope.with_session_state();
    }
    if enabled_tools.iter().any(|name| name == Fetch::NAME) {
        base_envelope = base_envelope.with_fetch_host("*");
    }
    base_envelope
        .validate_no_write_or_command()
        .map_err(|err| ApiError::Internal(format!("invalid base envelope: {err}")))?;

    #[cfg(test)]
    let build_metrics = Phase20BuildMetrics {
        adapter_instances: 1,
        transport_pairs: 1,
    };

    let (client, server) = LancetTransport::pair(256);
    let audit = if in_memory_audit {
        Arc::new(
            CepAuditStore::in_memory()
                .map_err(|err| ApiError::Internal(format!("audit store setup: {err}")))?,
        )
    } else {
        let audit_path = Path::new(project_root)
            .join(".lattice")
            .join("cep_audit.db");
        Arc::new(
            CepAuditStore::open(&audit_path)
                .map_err(|err| ApiError::Internal(format!("audit store setup: {err}")))?,
        )
    };
    let audit_context = Arc::new(RwLock::new(CepAuditContext::default()));
    let server_config = ServerRunConfig {
        audit: Arc::clone(&audit),
        audit_context: Arc::clone(&audit_context),
        base_envelope: base_envelope.clone(),
    };

    let mut registry = MethodRegistry::new();
    register_phase20_legacy_backend(
        &mut registry,
        project_root,
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|err| ApiError::Internal(format!("http client setup: {err}")))?,
    )
    .map_err(|err| ApiError::Internal(format!("legacy backend registration: {err:?}")))?;

    let server_shutdown = server.shutdown_handle();
    let server_task = tokio::spawn(async move {
        let _ = server.run(registry, server_config).await;
    });

    let enabled_names: HashSet<String> = tool_definitions
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    let lancet_client = client.clone();
    let adapter = Arc::new(LancetExecutorAdapter::new(client, enabled_names));
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: adapter,
    }));

    Ok(ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: LancetTurnRuntime {
            server_task,
            shutdown: server_shutdown,
        },
        lancet_client,
        audit,
        audit_context,
        #[cfg(test)]
        build_metrics,
    })
}

/// Map a CEP transport notification into an internal agent turn event.
pub(crate) fn map_cep_tool_event(event: ToolEvent) -> Option<AgentTurnEvent> {
    let params = event.params;
    let call_id = params.get("call_id").and_then(|v| v.as_u64())?;
    match event.method.as_str() {
        "cep.call_started" => Some(AgentTurnEvent::CepCallStarted {
            call_id,
            method: string_param(&params, "method")?,
            impact_kind: string_param(&params, "impact_kind").unwrap_or_else(|| "unknown".into()),
            audit_id: params.get("audit_id").and_then(|v| v.as_i64())?,
            semantic_stage: string_param(&params, "semantic_stage")?,
            guard_stage: string_param(&params, "guard_stage")?,
        }),
        "cep.call_finished" => {
            let method = string_param(&params, "method")?;
            Some(AgentTurnEvent::CepCallFinished {
                call_id,
                method: method.clone(),
                impact_kind: impact_kind_for_method(&method)
                    .map(impact_kind_label)
                    .unwrap_or("unknown")
                    .to_string(),
                audit_id: params.get("audit_id").and_then(|v| v.as_i64())?,
                ok: params.get("ok").and_then(|v| v.as_bool())?,
                semantic_stage: string_param(&params, "semantic_stage")?,
                guard_stage: string_param(&params, "guard_stage")?,
                error_code: params
                    .get("error_code")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
        }
        "cep.bridge_metadata" => Some(AgentTurnEvent::CepGuardVerdict {
            call_id,
            method: string_param(&params, "method")?,
            semantic_stage: string_param(&params, "semantic_stage")?,
            guard_stage: string_param(&params, "guard_stage")?,
            policy_enforced: params
                .get("policy_enforced")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            semantic_status: params
                .get("semantic_status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }),
        _ => None,
    }
}

fn string_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn adapter_wire_code(cep: &CepError) -> i32 {
    if let CepErrorData::Validation(data) = &cep.data {
        if data.reason == "invalid params" {
            return jsonrpc::INVALID_PARAMS;
        }
    }
    wire_code_for_error(cep)
}

fn retriability_label(value: &Retriability) -> String {
    match value {
        Retriability::Permanent => "permanent".to_string(),
        Retriability::Transient => "transient".to_string(),
        Retriability::RequiresReplan => "requires_replan".to_string(),
        Retriability::Unknown => "unknown".to_string(),
    }
}

fn schema_mismatch_for_method<M: CepMethod>(err: serde_json::Error) -> CepError {
    let field = extract_json_field_path(&err).unwrap_or_else(|| "params".to_string());
    let mut cep = invalid_params(format!("invalid params for {}: {err}", M::NAME));
    if let CepErrorData::Validation(data) = &mut cep.data {
        data.field = field;
    }
    cep
}

fn extract_json_field_path(err: &serde_json::Error) -> Option<String> {
    let message = err.to_string();
    if let Some(start) = message.find("missing field `") {
        let rest = &message[start + "missing field `".len()..];
        if let Some(end) = rest.find('`') {
            return Some(rest[..end].to_string());
        }
    }
    if let Some(start) = message.find("invalid type: ") {
        if let Some(field_start) = message[..start].rfind('`') {
            let prefix = &message[field_start + 1..];
            if let Some(field_end) = prefix.find('`') {
                return Some(prefix[..field_end].to_string());
            }
        }
    }
    None
}
