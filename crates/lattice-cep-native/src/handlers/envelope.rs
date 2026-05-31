//! Native CEP envelope.declare handler.
//!
//! Validates, stores, and manages active envelope lifecycle (declare, reject-double, supersede).

use crate::guard_l1::{normalize_write_path_pattern, WritePathPattern};
use crate::registry::{HandlerContext, MethodRegistry};
use crate::verify::{extract_verify_command, validate_verify_command};
use lattice_cep_catalog::methods::envelope::{Declare, DeclareParams, DeclareResult};
use lattice_cep_protocol::envelope::{Capability, ExecutionEnvelope};
use lattice_cep_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lattice_cep_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Register the native `envelope.declare` handler.
pub fn register_envelope_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Declare, _, _>(|params, ctx| async move {
        handle_declare(params, ctx).await
    })
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

fn envelope_already_active_error() -> CepError {
    CepError::new(
        CepErrorCode::EnvelopeAlreadyActive,
        "an active envelope already exists; use supersede=true to replace it",
        CepErrorData::Guard(GuardErrorData {
            method: "envelope.declare".into(),
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
                method: "envelope.declare".into(),
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
    use crate::envelope_store::ActiveEnvelopeStore;
    use crate::registry::{Cancellation, EventSink};
    use lattice_cep_catalog::CepMethod;
    use lattice_cep_protocol::envelope::{
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
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "declare test"),
                cep_context: Arc::new(crate::policy::CepTurnContext::defaults_for_project_root(
                    root,
                )),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                    crate::policy::default::PolicyConfig::defaults_for_project_root(
                        std::path::PathBuf::from(root),
                    ),
                ))
                    as Arc<dyn lattice_cep_protocol::policy::PolicyHook>,
                default_policy_hook: None,
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lattice_cep_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    fn test_ctx(
        root: &str,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
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
        let result = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: DeclareResult = serde_json::from_value(result).unwrap();
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
        registry
            .dispatch(Declare::NAME, serde_json::to_value(&params1).unwrap(), ctx1)
            .await
            .unwrap();

        // Second declare without supersede
        let (ctx2, _events2) = test_ctx_with_store(root, Arc::clone(&store));
        let params2 = DeclareParams {
            envelope: sample_envelope("second"),
            supersede: false,
            reason: None,
        };
        let err = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params2).unwrap(), ctx2)
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            lattice_cep_protocol::CepErrorCode::EnvelopeAlreadyActive
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
        registry
            .dispatch(Declare::NAME, serde_json::to_value(&params1).unwrap(), ctx1)
            .await
            .unwrap();

        let (ctx2, _events2) = test_ctx_with_store(root, Arc::clone(&store));
        let mut invalid = sample_envelope("invalid second");
        invalid.intent.description = String::new();
        let params2 = DeclareParams {
            envelope: invalid,
            supersede: false,
            reason: None,
        };
        let err = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params2).unwrap(), ctx2)
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            lattice_cep_protocol::CepErrorCode::EnvelopeAlreadyActive
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
        let result1 = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params1).unwrap(), ctx1)
            .await
            .unwrap();
        let parsed1: DeclareResult = serde_json::from_value(result1).unwrap();

        // Supersede
        let (ctx2, mut events2) = test_ctx_with_store(root, Arc::clone(&store));
        let params2 = DeclareParams {
            envelope: sample_envelope("replacement"),
            supersede: true,
            reason: Some("upgrade".into()),
        };
        let result2 = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params2).unwrap(), ctx2)
            .await
            .unwrap();
        let parsed2: DeclareResult = serde_json::from_value(result2).unwrap();
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
        registry
            .dispatch(Declare::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();

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
        let err = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, lattice_cep_protocol::CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn handle_declare_rejects_mutation_verify_command_at_declare_time() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let mut envelope = sample_envelope("bad verify");
        // cargo fmt is classified as WorkspaceMutation by command policy
        envelope.verify = Some(lattice_cep_protocol::envelope::VerifyCommand {
            command: "cargo fmt".into(),
            expected_exit_code: Some(0),
        });

        let params = DeclareParams {
            envelope,
            supersede: false,
            reason: None,
        };
        let err = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, lattice_cep_protocol::CepErrorCode::GuardDenied);
    }

    #[tokio::test]
    async fn handle_declare_accepts_diagnostic_verify_command() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let mut envelope = sample_envelope("good verify");
        // echo is classified as Diagnostic by command policy
        envelope.verify = Some(lattice_cep_protocol::envelope::VerifyCommand {
            command: "echo ok".into(),
            expected_exit_code: Some(0),
        });

        let params = DeclareParams {
            envelope,
            supersede: false,
            reason: None,
        };
        let result = registry
            .dispatch(Declare::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: DeclareResult = serde_json::from_value(result).unwrap();
        assert!(parsed.accepted);
    }
}
