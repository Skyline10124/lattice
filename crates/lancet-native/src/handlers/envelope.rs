// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP session.envelope handler.
//!
//! Validates, stores, and manages active envelope lifecycle (declare, reject-double, supersede).

use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::enforcement::guard_l1::{WritePathPattern, normalize_write_path_pattern};
use crate::enforcement::verify::{extract_verify_command, validate_verify_command};
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::envelope::{
    DeclareParams, DeclareResult, Envelope, EnvelopeParams, EnvelopeResult,
};
use lancet_protocol::envelope::{Capability, ExecutionEnvelope, ImpactKind};
use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Register the native `session.envelope` handler.
pub fn register_envelope_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Envelope, _, _>(handle_envelope)
}

async fn handle_envelope(
    params: EnvelopeParams,
    ctx: HandlerContext,
) -> Result<EnvelopeResult, CepError> {
    match params {
        EnvelopeParams::Declare(params) => handle_declare(params, ctx)
            .await
            .map(EnvelopeResult::Declare),
    }
}

async fn handle_declare(
    params: DeclareParams,
    ctx: HandlerContext,
) -> Result<DeclareResult, CepError> {
    // Use the turn id from HandlerContext so the envelope metadata aligns with audit rows.
    let turn_id = &ctx.turn_id;
    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    if !params.supersede && ctx.active_envelope.active_envelope_id().await.is_some() {
        return Err(envelope_already_active_error());
    }

    let _evaluation = evaluate_and_enforce_policy(&ctx, Envelope::NAME, &params).await?;

    validate_envelope_boundaries(&params.envelope, &project_root)?;

    // Validate the verify command at declare time if present.
    if let Some(verify_cmd) = extract_verify_command(&params.envelope) {
        let policy = &ctx.cep_context.policy.command;
        validate_verify_command(&verify_cmd, policy, &project_root)?;
    }

    if params.supersede {
        let outcome = ctx
            .active_envelope
            .declare_with_supersede(
                params.envelope.clone(),
                ctx.call_id,
                turn_id,
                params.reason.clone(),
            )
            .await?;

        record_session_state_effect(&ctx);

        // Emit superseded event if there was a previous envelope
        if let Some(ref old_id) = outcome.superseded_envelope_id {
            ctx.events
                .emit(
                    "cep.envelope_superseded",
                    json!({
                        "old_envelope_id": old_id,
                        "new_envelope_id": outcome.envelope_id,
                        "reason": params.reason,
                    }),
                )
                .await;
        }

        // Emit declared event
        ctx.events
            .emit(
                "cep.envelope_declared",
                json!({
                    "envelope_id": outcome.envelope_id,
                    "envelope_hash": outcome.envelope_hash,
                }),
            )
            .await;

        // Store canonical envelope JSON once so later audit finalization can reuse it.
        let canonical_json = serde_json::to_string(&params.envelope).unwrap_or_default();
        ctx.record_durable_metadata(json!({
            "envelope_id": outcome.envelope_id,
            "envelope_hash": outcome.envelope_hash,
            "supersede": true,
            "superseded_envelope_id": outcome.superseded_envelope_id,
            "active_envelope_id": outcome.envelope_id,
            "active_envelope_hash": outcome.envelope_hash,
            "canonical_envelope_json": canonical_json,
        }));

        Ok(DeclareResult {
            envelope_id: outcome.envelope_id,
            envelope_hash: outcome.envelope_hash,
            accepted: outcome.accepted,
            superseded_envelope_id: outcome.superseded_envelope_id,
        })
    } else {
        let outcome = ctx
            .active_envelope
            .declare(params.envelope.clone(), ctx.call_id, turn_id)
            .await?;

        record_session_state_effect(&ctx);

        // Emit declared event
        ctx.events
            .emit(
                "cep.envelope_declared",
                json!({
                    "envelope_id": outcome.envelope_id,
                    "envelope_hash": outcome.envelope_hash,
                }),
            )
            .await;

        // Store canonical envelope JSON once so later audit finalization can reuse it.
        let canonical_json = serde_json::to_string(&params.envelope).unwrap_or_default();
        ctx.record_durable_metadata(json!({
            "envelope_id": outcome.envelope_id,
            "envelope_hash": outcome.envelope_hash,
            "supersede": false,
            "active_envelope_id": outcome.envelope_id,
            "active_envelope_hash": outcome.envelope_hash,
            "canonical_envelope_json": canonical_json,
        }));

        Ok(DeclareResult {
            envelope_id: outcome.envelope_id,
            envelope_hash: outcome.envelope_hash,
            accepted: outcome.accepted,
            superseded_envelope_id: None,
        })
    }
}

fn record_session_state_effect(ctx: &HandlerContext) {
    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::SessionStateMutation,
        scope: ObservedScope::SessionState,
        required_capability: RequiredCapability::MutateSessionState,
        source: EffectSource::HandlerBackend,
    });
}

fn envelope_already_active_error() -> CepError {
    CepError::new(
        CepErrorCode::EnvelopeAlreadyActive,
        "an active envelope already exists; use supersede=true to replace it",
        CepErrorData::Guard(GuardErrorData {
            method: Envelope::NAME.into(),
            reason: "envelope already active".into(),
        }),
        Retriability::Permanent,
    )
}

pub(crate) fn validate_envelope_boundaries(
    envelope: &ExecutionEnvelope,
    project_root: &str,
) -> Result<(), CepError> {
    let root_canonical = std::fs::canonicalize(project_root).map_err(|err| {
        CepError::new(
            CepErrorCode::ProjectRootViolation,
            format!("cannot canonicalize project root '{project_root}': {err}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "project_root".into(),
                reason: err.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    for cap in &envelope.capabilities {
        if let Capability::WritePath { path_pattern } = cap {
            validate_write_path_boundary(path_pattern, &root_canonical)?;
        }
    }
    Ok(())
}

fn validate_write_path_boundary(pattern: &str, root_canonical: &Path) -> Result<(), CepError> {
    let normalized = normalize_write_path_pattern(pattern)?;
    let base = match normalized {
        WritePathPattern::ExactFile(path) => path,
        WritePathPattern::DirectoryGlob(path) => path,
    };
    let base_resolved = root_canonical.join(base);
    let existing_prefix = canonicalize_existing_prefix(&base_resolved)?;
    if existing_prefix != root_canonical && !existing_prefix.starts_with(root_canonical) {
        return Err(CepError::new(
            CepErrorCode::ProjectRootViolation,
            format!(
                "WritePath pattern '{}' escapes project root via '{}'",
                pattern,
                existing_prefix.display()
            ),
            CepErrorData::Guard(GuardErrorData {
                method: Envelope::NAME.into(),
                reason: "WritePath scope escapes project root".into(),
            }),
            Retriability::Permanent,
        ));
    }
    Ok(())
}

fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, CepError> {
    let mut current = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            return Ok(canonical);
        }
        if !current.pop() {
            return Err(CepError::new(
                CepErrorCode::PathContainment,
                format!(
                    "cannot resolve any existing prefix for '{}'",
                    path.display()
                ),
                CepErrorData::Validation(ValidationErrorData {
                    field: "capabilities.WritePath.path_pattern".into(),
                    reason: "no existing path prefix".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::envelope_store::ActiveEnvelopeStore;
    use lancet_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact,
        ImpactConfidence, ImpactKind, Intent, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "declare test")
                    .with_session_state(),
                cep_context: Arc::new(
                    lancet_authority::policy::CepTurnContext::defaults_for_project_root(root),
                ),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                    lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                        std::path::PathBuf::from(root),
                    ),
                )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: None,
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            },
            event_rx,
        )
    }

    fn test_ctx(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        test_ctx_with_store(root, Arc::new(ActiveEnvelopeStore::new()))
    }

    fn sample_envelope(description: &str) -> ExecutionEnvelope {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: description.into(),
            },
            capabilities,
            scope: Scope {
                workspace_root: Some("/tmp/test".into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceMutation,
                target: "src/**".into(),
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        }
    }

    fn test_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_envelope_methods(&mut registry).unwrap();
        registry
    }

    async fn dispatch_declare(
        registry: &MethodRegistry,
        params: DeclareParams,
        ctx: HandlerContext,
    ) -> Result<DeclareResult, CepError> {
        let result = registry
            .dispatch(
                Envelope::NAME,
                serde_json::to_value(EnvelopeParams::Declare(params)).unwrap(),
                ctx,
            )
            .await?;
        let EnvelopeResult::Declare(result) = serde_json::from_value(result).unwrap();
        Ok(result)
    }

    #[tokio::test]
    async fn handle_declare_first_call_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let params = DeclareParams {
            envelope: sample_envelope("first declare"),
            supersede: false,
            reason: None,
        };
        let parsed = dispatch_declare(&registry, params, ctx).await.unwrap();
        assert!(parsed.accepted);
        assert!(!parsed.envelope_hash.is_empty());
        assert!(parsed.superseded_envelope_id.is_none());
    }

    #[tokio::test]
    async fn handle_declare_double_without_supersede_returns_already_active() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // First declare
        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let params1 = DeclareParams {
            envelope: sample_envelope("first"),
            supersede: false,
            reason: None,
        };
        dispatch_declare(&registry, params1, ctx1).await.unwrap();

        // Second declare without supersede
        let (ctx2, _events2) = test_ctx_with_store(root, Arc::clone(&store));
        let params2 = DeclareParams {
            envelope: sample_envelope("second"),
            supersede: false,
            reason: None,
        };
        let err = dispatch_declare(&registry, params2, ctx2)
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            lancet_protocol::CepErrorCode::EnvelopeAlreadyActive
        );
    }

    #[tokio::test]
    async fn handle_declare_second_invalid_payload_still_returns_already_active() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let params1 = DeclareParams {
            envelope: sample_envelope("first"),
            supersede: false,
            reason: None,
        };
        dispatch_declare(&registry, params1, ctx1).await.unwrap();

        let (ctx2, _events2) = test_ctx_with_store(root, Arc::clone(&store));
        let mut invalid = sample_envelope("invalid second");
        invalid.intent.description = String::new();
        let params2 = DeclareParams {
            envelope: invalid,
            supersede: false,
            reason: None,
        };
        let err = dispatch_declare(&registry, params2, ctx2)
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            lancet_protocol::CepErrorCode::EnvelopeAlreadyActive
        );
    }

    #[tokio::test]
    async fn handle_declare_supersede_replaces_active() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // First declare
        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let params1 = DeclareParams {
            envelope: sample_envelope("original"),
            supersede: false,
            reason: None,
        };
        let parsed1 = dispatch_declare(&registry, params1, ctx1).await.unwrap();

        // Supersede
        let (ctx2, mut events2) = test_ctx_with_store(root, Arc::clone(&store));
        let params2 = DeclareParams {
            envelope: sample_envelope("replacement"),
            supersede: true,
            reason: Some("upgrade".into()),
        };
        let parsed2 = dispatch_declare(&registry, params2, ctx2).await.unwrap();
        assert!(parsed2.accepted);
        assert_eq!(
            parsed2.superseded_envelope_id,
            Some(parsed1.envelope_id.clone())
        );

        // Check events
        let mut found_superseded = false;
        let mut found_declared = false;
        while let Ok(event) = events2.try_recv() {
            if event.method == "cep.envelope_superseded" {
                found_superseded = true;
            }
            if event.method == "cep.envelope_declared" {
                found_declared = true;
            }
        }
        assert!(found_superseded, "should emit cep.envelope_superseded");
        assert!(found_declared, "should emit cep.envelope_declared");
    }

    #[tokio::test]
    async fn handle_declare_emits_envelope_declared_event() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, mut events) = test_ctx(root);

        let params = DeclareParams {
            envelope: sample_envelope("event test"),
            supersede: false,
            reason: None,
        };
        dispatch_declare(&registry, params, ctx).await.unwrap();

        let mut found_declared = false;
        while let Ok(event) = events.try_recv() {
            if event.method == "cep.envelope_declared" {
                found_declared = true;
                assert!(event.params["envelope_id"].as_str().is_some());
                assert!(event.params["envelope_hash"].as_str().is_some());
            }
        }
        assert!(found_declared, "should emit cep.envelope_declared");
    }

    #[tokio::test]
    async fn handle_declare_rejects_invalid_envelope() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let mut envelope = sample_envelope("invalid");
        envelope.intent.description = String::new();

        let params = DeclareParams {
            envelope,
            supersede: false,
            reason: None,
        };
        let err = dispatch_declare(&registry, params, ctx).await.unwrap_err();
        assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn handle_declare_rejects_mutation_verify_command_at_declare_time() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let mut envelope = sample_envelope("bad verify");
        // cargo fmt is classified as WorkspaceMutation by command policy
        envelope.verify = Some(lancet_protocol::envelope::VerifyCommand {
            command: "cargo fmt".into(),
            expected_exit_code: Some(0),
        });

        let params = DeclareParams {
            envelope,
            supersede: false,
            reason: None,
        };
        let err = dispatch_declare(&registry, params, ctx).await.unwrap_err();
        assert_eq!(err.code, lancet_protocol::CepErrorCode::GuardDenied);
    }

    #[tokio::test]
    async fn handle_declare_accepts_diagnostic_verify_command() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let mut envelope = sample_envelope("good verify");
        // echo is classified as Diagnostic by command policy
        envelope.verify = Some(lancet_protocol::envelope::VerifyCommand {
            command: "echo ok".into(),
            expected_exit_code: Some(0),
        });

        let params = DeclareParams {
            envelope,
            supersede: false,
            reason: None,
        };
        let parsed = dispatch_declare(&registry, params, ctx).await.unwrap();
        assert!(parsed.accepted);
    }
}
