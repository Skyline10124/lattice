use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::guard_l1::evaluate_and_enforce_policy;
use crate::handlers::envelope::validate_envelope_boundaries;
use crate::impact::{AnalysisResult, ImpactAnalyzer};
use crate::invariants::{opaque_notes, InvariantCard};
use crate::registry::{HandlerContext, MethodRegistry};
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::envelope::{
    L2Status, SubmitIntent, SubmitIntentParams, SubmitIntentResult,
};
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::{
    Capability, Constraints, ExecutionEnvelope, ImpactKind, Intent, Scope,
};
use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::collections::BTreeSet;

pub fn register_submit_intent(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<SubmitIntent, _, _>(|params, ctx| async move {
        handle_submit_intent(params, ctx).await
    })
}

async fn handle_submit_intent(
    params: SubmitIntentParams,
    ctx: HandlerContext,
) -> Result<SubmitIntentResult, CepError> {
    let turn_id = &ctx.turn_id;

    validate_target(&params.target, &ctx)?;

    let _evaluation = evaluate_and_enforce_policy(&ctx, SubmitIntent::NAME, &params).await?;

    let method_name = resolve_method_name(&params.action, &params.target);

    if !params.supersede && ctx.active_envelope.active_envelope_id().await.is_some() {
        return Err(envelope_already_active_error());
    }

    let analyzer = ImpactAnalyzer::new();
    let analysis = analyzer.analyze(&method_name, &params.target, params.params.as_ref());

    let matched_cards = ctx.invariant_index.match_target(&params.target);
    let card_ids: Vec<String> = matched_cards.iter().map(|c| c.id.clone()).collect();

    let envelope = build_derived_envelope(&params, &analysis, &matched_cards);

    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();
    validate_envelope_boundaries(&envelope, &project_root)?;

    let canonical_json = serde_json::to_string(&envelope).unwrap_or_default();

    let outcome = if params.supersede {
        ctx.active_envelope
            .declare_with_supersede(envelope, ctx.call_id, turn_id, params.reason.clone())
            .await?
    } else {
        ctx.active_envelope
            .declare(envelope, ctx.call_id, turn_id)
            .await?
    };

    record_session_state_effect(&ctx);

    ctx.events
        .emit(
            "cep.envelope_derived",
            json!({
                "envelope_id": outcome.envelope_id,
                "confidence": serde_json::to_string(&analysis.confidence).unwrap_or_default(),
                "matched_cards": card_ids,
            }),
        )
        .await;

    ctx.record_durable_metadata(json!({
        "envelope_id": outcome.envelope_id,
        "envelope_hash": outcome.envelope_hash,
        "supersede": params.supersede,
        "derived": true,
        "derivation_source": "submit_intent",
        "confidence": serde_json::to_string(&analysis.confidence).unwrap_or_default(),
        "matched_cards": card_ids,
        "canonical_envelope_json": canonical_json,
    }));

    Ok(SubmitIntentResult {
        envelope_id: outcome.envelope_id,
        envelope_hash: outcome.envelope_hash,
        accepted: outcome.accepted,
        impact: analysis.impact,
        matched_cards: card_ids,
        l2_coverage: L2Status::NotApplicable,
        superseded_envelope_id: outcome.superseded_envelope_id,
    })
}

fn validate_target(target: &str, ctx: &HandlerContext) -> Result<(), CepError> {
    if target.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "target must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "target".into(),
                reason: "empty target".into(),
            }),
            Retriability::Permanent,
        ));
    }

    if target.contains("..") {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            format!("target contains path traversal: {target}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "target".into(),
                reason: "path traversal (..)".into(),
            }),
            Retriability::Permanent,
        ));
    }

    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();
    if target.starts_with('/') {
        let root_canonical = std::fs::canonicalize(&project_root).map_err(|err| {
            CepError::new(
                CepErrorCode::ProjectRootViolation,
                format!("cannot canonicalize project root: {err}"),
                CepErrorData::Validation(ValidationErrorData {
                    field: "project_root".into(),
                    reason: err.to_string(),
                }),
                Retriability::Permanent,
            )
        })?;
        let target_path = std::path::Path::new(target);
        if let Ok(canonical) = std::fs::canonicalize(target_path) {
            if !canonical.starts_with(&root_canonical) {
                return Err(CepError::new(
                    CepErrorCode::ProjectRootViolation,
                    format!("target '{target}' is outside project root"),
                    CepErrorData::Guard(GuardErrorData {
                        method: "envelope.submit_intent".into(),
                        reason: "target outside project root".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }

    Ok(())
}

fn resolve_method_name(action: &str, target: &str) -> String {
    match action {
        "write" | "write_file" => "workspace.write_file".to_string(),
        "patch" => "workspace.patch".to_string(),
        "run" | "execute" | "command" => "command.run".to_string(),
        "read" | "read_file" => "workspace.read_file".to_string(),
        "grep" | "search" => "workspace.grep".to_string(),
        "list" | "ls" | "list_directory" => "workspace.list_directory".to_string(),
        "find" | "find_files" => "workspace.find_files".to_string(),
        "fetch" => "web.fetch".to_string(),
        "todo" | "todo_write" => "session.todo_write".to_string(),
        "declare" | "declare_envelope" => "envelope.declare".to_string(),
        _ => heuristic_fallback(target),
    }
}

fn heuristic_fallback(target: &str) -> String {
    if target.starts_with("http") {
        "web.fetch".to_string()
    } else if target.contains('.') && !target.ends_with('/') {
        "workspace.write_file".to_string()
    } else if target.contains('/') {
        "workspace.list_directory".to_string()
    } else {
        "command.run".to_string()
    }
}

fn build_derived_envelope(
    params: &SubmitIntentParams,
    analysis: &AnalysisResult,
    matched_cards: &[&InvariantCard],
) -> ExecutionEnvelope {
    let impact_kind = &analysis.impact.kind;
    let scope = &analysis.scope;

    let mut capabilities = BTreeSet::new();
    match impact_kind {
        ImpactKind::WorkspaceMutation => {
            capabilities.insert(Capability::WritePath {
                path_pattern: params.target.clone(),
            });
        }
        ImpactKind::CommandExecution => {
            capabilities.insert(Capability::RunCommand {
                argv_allowlist: vec![params.target.clone()],
            });
        }
        ImpactKind::ExternalObservation => {
            capabilities.insert(Capability::FetchUrl {
                host: params.target.clone(),
            });
        }
        ImpactKind::WorkspaceObservation => {
            capabilities.insert(Capability::ReadPath {
                path_pattern: params.target.clone(),
            });
        }
        ImpactKind::SessionStateMutation => {
            capabilities.insert(Capability::MutateSessionState);
        }
        ImpactKind::AuditObservation => {
            capabilities.insert(Capability::ReadAudit);
        }
    }

    let scope_path = match scope {
        lancet_protocol::envelope::ImpactScope::ExactFile(p) => p.to_string_lossy().to_string(),
        lancet_protocol::envelope::ImpactScope::Directory(p) => p.to_string_lossy().to_string(),
        lancet_protocol::envelope::ImpactScope::FileSet(paths) => paths
            .first()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        lancet_protocol::envelope::ImpactScope::Host(_) => params.target.clone(),
        lancet_protocol::envelope::ImpactScope::SessionState => String::new(),
    };

    let mut extensions = serde_json::Map::new();
    extensions.insert("derived".to_string(), serde_json::Value::Bool(true));
    extensions.insert(
        "derivation_source".to_string(),
        serde_json::Value::String("submit_intent".to_string()),
    );
    extensions.insert(
        "confidence".to_string(),
        serde_json::to_value(analysis.confidence).unwrap_or(serde_json::Value::Null),
    );
    if !analysis.unknowns.is_empty() {
        extensions.insert(
            "unknowns".to_string(),
            serde_json::to_value(&analysis.unknowns).unwrap_or(serde_json::Value::Null),
        );
    }
    extensions.insert(
        "matched_cards".to_string(),
        serde_json::Value::Array(
            matched_cards
                .iter()
                .map(|c| serde_json::Value::String(c.id.clone()))
                .collect(),
        ),
    );
    let notes = opaque_notes(matched_cards);
    if !notes.is_empty() {
        extensions.insert("opaque_notes".to_string(), serde_json::Value::Array(notes));
    }

    ExecutionEnvelope {
        intent: Intent {
            description: format!("{} {}", params.action, params.target),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(scope_path),
            extensions: serde_json::Map::new(),
        },
        impact: vec![analysis.impact.clone()],
        constraints: build_structural_constraints(matched_cards),
        verify: None,
        on_failure: None,
        extensions,
    }
}

fn build_structural_constraints(matched_cards: &[&InvariantCard]) -> Constraints {
    let forbidden: Vec<serde_json::Value> = matched_cards
        .iter()
        .flat_map(|card| {
            card.forbidden.iter().map(|entry| {
                json!({
                    "card_id": card.id,
                    "category": entry.category,
                    "reason": entry.reason,
                })
            })
        })
        .collect();
    let mut extensions = serde_json::Map::new();
    if !forbidden.is_empty() {
        extensions.insert("forbidden".to_string(), serde_json::Value::Array(forbidden));
    }
    Constraints {
        extensions,
        ..Constraints::default()
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
            method: "envelope.submit_intent".into(),
            reason: "envelope already active".into(),
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope_store::ActiveEnvelopeStore;
    use crate::invariants::{CardAnchor, ForbiddenEntry, InvariantCard, InvariantCardIndex};
    use crate::registry::{Cancellation, EventSink};
    use lancet_catalog::CepMethod;
    use lancet_protocol::envelope::{BaseExecutionEnvelope, ImpactConfidence};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_submit_intent(&mut registry).unwrap();
        registry
    }

    fn test_ctx_with_index(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
        invariant_index: Arc<InvariantCardIndex>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(
                    root,
                    "submit_intent test",
                )
                .with_session_state(),
                cep_context: Arc::new(crate::policy::CepTurnContext::defaults_for_project_root(
                    root,
                )),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index,
                cep_mode: lancet_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    fn test_ctx(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        test_ctx_with_index(
            root,
            Arc::new(ActiveEnvelopeStore::new()),
            Arc::new(InvariantCardIndex::new(vec![])),
        )
    }

    #[tokio::test]
    async fn test_submit_intent_derives_envelope_for_write_target() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let params = SubmitIntentParams {
            action: "write".to_string(),
            target: "src/main.rs".to_string(),
            params: None,
            supersede: false,
            reason: None,
        };
        let result = registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx,
            )
            .await
            .unwrap();
        let parsed: SubmitIntentResult = serde_json::from_value(result).unwrap();
        assert!(parsed.accepted);
        assert!(!parsed.envelope_id.is_empty());
        assert!(!parsed.envelope_hash.is_empty());
        assert_eq!(parsed.impact.confidence, ImpactConfidence::Complete);
        assert!(matches!(parsed.l2_coverage, L2Status::NotApplicable));
    }

    #[tokio::test]
    async fn test_submit_intent_with_invariant_card_adds_constraints() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        let card = InvariantCard {
            id: "INV-TEST".into(),
            title: "No structs".into(),
            anchor: CardAnchor::Path("src/main.rs".into()),
            forbidden: vec![ForbiddenEntry {
                category: "struct_def".into(),
                reason: "Structs frozen in alpha".into(),
            }],
            opaque: vec![],
        };
        let (ctx, _events) = test_ctx_with_index(
            root,
            Arc::clone(&store),
            Arc::new(InvariantCardIndex::new(vec![card])),
        );

        let params = SubmitIntentParams {
            action: "write".to_string(),
            target: "src/main.rs".to_string(),
            params: None,
            supersede: false,
            reason: None,
        };
        registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx,
            )
            .await
            .unwrap();

        let (_, envelope) = store.active_envelope().await.unwrap();
        let forbidden = envelope
            .constraints
            .extensions
            .get("forbidden")
            .and_then(|v| v.as_array())
            .expect("forbidden constraints");
        assert_eq!(forbidden.len(), 1);
        assert_eq!(forbidden[0]["category"], "struct_def");
        assert_eq!(envelope.extensions["confidence"], "complete");
    }

    #[tokio::test]
    async fn test_submit_intent_already_active_without_supersede() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        let index = Arc::new(InvariantCardIndex::new(vec![]));

        let (ctx1, _events1) = test_ctx_with_index(root, Arc::clone(&store), Arc::clone(&index));
        let params = SubmitIntentParams {
            action: "write".to_string(),
            target: "src/main.rs".to_string(),
            params: None,
            supersede: false,
            reason: None,
        };
        registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx1,
            )
            .await
            .unwrap();

        let (ctx2, _events2) = test_ctx_with_index(root, store, index);
        let err = registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx2,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::EnvelopeAlreadyActive);
    }

    #[tokio::test]
    async fn test_submit_intent_supersede_replaces() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        let index = Arc::new(InvariantCardIndex::new(vec![]));

        let (ctx1, _events1) = test_ctx_with_index(root, Arc::clone(&store), Arc::clone(&index));
        let first = SubmitIntentParams {
            action: "write".into(),
            target: "src/one.rs".into(),
            params: None,
            supersede: false,
            reason: None,
        };
        registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&first).unwrap(),
                ctx1,
            )
            .await
            .unwrap();
        let first_id = store.active_envelope_id().await.unwrap();

        let (ctx2, _events2) = test_ctx_with_index(root, Arc::clone(&store), index);
        let second = SubmitIntentParams {
            action: "write".into(),
            target: "src/two.rs".into(),
            params: None,
            supersede: true,
            reason: Some("new target".into()),
        };
        let result = registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&second).unwrap(),
                ctx2,
            )
            .await
            .unwrap();
        let parsed: SubmitIntentResult = serde_json::from_value(result).unwrap();
        assert_eq!(
            parsed.superseded_envelope_id.as_deref(),
            Some(first_id.as_str())
        );
        assert_ne!(store.active_envelope_id().await.unwrap(), first_id);
    }

    #[tokio::test]
    async fn test_submit_intent_invalid_target_returns_validation_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let params = SubmitIntentParams {
            action: "write".to_string(),
            target: "".to_string(),
            params: None,
            supersede: false,
            reason: None,
        };
        let err = registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn test_submit_intent_invalid_dotdot_target_returns_validation_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = test_registry();
        let (ctx, _events) = test_ctx(root);

        let params = SubmitIntentParams {
            action: "write".to_string(),
            target: "../escape.rs".to_string(),
            params: None,
            supersede: false,
            reason: None,
        };
        let err = registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&params).unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }
}
