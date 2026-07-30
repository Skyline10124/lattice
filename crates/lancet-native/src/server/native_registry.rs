// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native catalog method registration.

use crate::handlers::{
    register_external_methods, register_mutation_methods, register_workspace_observation_methods,
};
use crate::server::registry::MethodRegistry;
use lancet_protocol::CepError;

/// Register the remaining LANCET native handlers (observation, external).
///
/// `session.todo_write` is deliberately absent: #29 migrated it to an embedded
/// Component Tool and deleted its native handler. There is no native fallback —
/// a runtime whose composition does not embed the Component has no other way to
/// serve the method, and dispatch reports `MethodNotFound`.
pub fn register_cep_native_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_native_workspace_observation(registry)?;
    register_native_external(registry)?;
    Ok(())
}

/// Register native CEP workspace observation handlers.
pub fn register_native_workspace_observation(
    registry: &mut MethodRegistry,
) -> Result<(), CepError> {
    register_workspace_observation_methods(registry)
}

/// Register native CEP `fetch.web` handler.
pub fn register_native_external(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_external_methods(registry)
}

/// Register native CEP workspace mutation handlers.
pub fn register_native_mutation(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_mutation_methods(registry)
}

pub fn register_production_method_registry(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_cep_native_methods(registry)?;
    register_native_mutation(registry)
}

#[cfg(test)]
mod cep_native_registry_tests {
    use super::*;
    use crate::audit::{GUARD_STAGE_NATIVE, SEMANTIC_STAGE_NATIVE};
    use crate::server::registry::{Cancellation, EventSink, HandlerContext};
    use crate::workspace::LocalWorkspace;
    use crate::{CepServerNativeExt, CepTransport, ServerRunConfig};
    use lancet_authority::policy::CepTurnContext;
    use lancet_catalog::CepMethod;
    use lancet_catalog::catalog;
    use lancet_catalog::methods::workspace::{
        Create, CreateParams, Edit, EditOperation, EditParams, FindFilesParams, ObserveWorkspace,
        ObserveWorkspaceParams, ObserveWorkspaceResult, ReadFileParams,
    };
    use lancet_protocol::CepErrorCode;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn production_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        registry
    }

    #[test]
    fn cep_native_registry_registers_full_catalog_once() {
        let registry = production_registry();
        for descriptor in catalog() {
            assert!(
                registry.contains(descriptor.name),
                "missing catalog method {}",
                descriptor.name
            );
        }
    }

    /// #71: both effect-kind layers key on `lancet_catalog::catalog()` — the
    /// pre-dispatch declared check and the post-dispatch observed check each
    /// fall through when the method has no descriptor. A production method
    /// registered outside the catalog would therefore be exempt from
    /// `[lancet.effects] deny` entirely, so the registry must not contain one.
    ///
    /// This is a real subset check over `registered_methods()`, not an
    /// allowlist of known-bad names: any future registration outside the
    /// catalog fails it. Adding a native method means adding its descriptor to
    /// the catalog too.
    #[test]
    fn cep_native_registry_registers_nothing_outside_the_catalog() {
        let registry = production_registry();
        let catalog_names: Vec<&str> = catalog().iter().map(|d| d.name).collect();
        let registered = registry.registered_methods();

        assert!(
            !registered.is_empty(),
            "an empty registry would make this subset check vacuous"
        );
        let orphans: Vec<&str> = registered
            .iter()
            .copied()
            .filter(|name| !catalog_names.contains(name))
            .collect();
        assert!(
            orphans.is_empty(),
            "registered without a catalog descriptor, so exempt from both \
             effect-kind deny layers: {orphans:?}"
        );
    }

    #[test]
    fn cep_native_registry_double_registration_fails() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        let err =
            register_cep_native_methods(&mut registry).expect_err("double native registration");
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }

    #[tokio::test]
    async fn cep_native_registry_native_error_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "x").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
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
            cep_mode: lancet_protocol::policy::CepMode::Off,
            workspace: Arc::new(LocalWorkspace::new(std::path::Path::new(root))),
        };

        let params = ReadFileParams {
            path: "../outside".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::ReadFile(params)).unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[tokio::test]
    async fn cep_native_registry_labels_native_methods() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        config.policy_hook = default_hook.clone();
        config.default_policy_hook = Some(default_hook);

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        client
            .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }))
            .await
            .unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        let read_row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(read_row.method, ObserveWorkspace::NAME);
        assert_eq!(read_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert_eq!(read_row.guard_stage, GUARD_STAGE_NATIVE);
        assert!(read_row.policy_enforced);
        let metadata: serde_json::Value =
            serde_json::from_str(read_row.metadata_json.as_deref().expect("metadata json"))
                .unwrap();
        assert_eq!(
            metadata["visibility_evidence"],
            serde_json::json!({"included_ignored": false, "filtered_by": []})
        );

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn recursive_visibility_evidence_survives_transport_finalization() {
        let dir = TempDir::new().unwrap();
        gix::init(dir.path()).expect("gix init");
        std::fs::write(dir.path().join(".gitignore"), "secret/\n").unwrap();
        std::fs::create_dir(dir.path().join("secret")).unwrap();
        std::fs::write(dir.path().join("secret/key"), "AUDIT_SENTINEL\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
        let mut policy_config =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(root.into());
        policy_config.workspace.included_ignored_read_patterns = vec!["secret/key".into()];
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_config,
        ));
        config.policy_hook = default_hook.clone();
        config.default_policy_hook = Some(default_hook);

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        let result = client
            .call::<ObserveWorkspace>(ObserveWorkspaceParams::FindFiles(FindFilesParams {
                pattern: "*".into(),
                path: Some(".".into()),
                limit: Some(50),
            }))
            .await
            .unwrap();
        let ObserveWorkspaceResult::FindFiles(result) = result else {
            panic!("expected find result");
        };
        assert!(result.paths.iter().any(|path| path == "secret/key"));
        assert!(!result.paths.iter().any(|path| path == "secret"));

        let row = audit.row_by_audit_id(1).unwrap();
        let raw = row.metadata_json.as_deref().expect("metadata json");
        let metadata: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            metadata["visibility_evidence"],
            serde_json::json!({
                "included_ignored": true,
                "filtered_by": ["system_exclusion", "ignored_default"],
            })
        );
        assert!(!raw.contains("secret/key"));
        assert!(!raw.contains("AUDIT_SENTINEL"));
        assert!(!raw.contains(root));

        drop(client);
        server_task.abort();
    }

    /// Server config whose turn policy denies `denied` and whose project root
    /// is `root`. Both the CEP turn context and the policy hook are rooted
    /// there so the only reason a call can fail is the effect-kind deny.
    fn config_with_denied_effect_kind(
        root: &str,
        denied: lancet_protocol::ImpactKind,
    ) -> ServerRunConfig {
        let mut config = ServerRunConfig::in_memory_for_tests();
        let mut policy = lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root);
        policy.denied_effect_kinds.insert(denied);
        config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        config.policy_hook = default_hook.clone();
        config.default_policy_hook = Some(default_hook);
        config
    }

    /// A pre-dispatch denial is distinguishable from the post-dispatch
    /// observed-effect denial: only the declared-kind check says "declared
    /// effect denied". Asserting the shared "denied by effect-kind policy"
    /// substring alone cannot tell the two layers apart (#71).
    fn assert_pre_dispatch_denial(err: &lancet_protocol::CepError) {
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        assert!(
            err.message.contains("declared effect denied"),
            "pre-dispatch denial must be the declared-kind layer, got: {}",
            err.message
        );
    }

    /// Issue #46 (Codeberg #24) slice 3: pre-dispatch effect-kind denial must
    /// never call the mutation backend. A denied `mutate.create` leaves no
    /// file and records a denied audit row.
    #[tokio::test]
    async fn denied_effect_kind_blocks_mutate_create_before_backend() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let config =
            config_with_denied_effect_kind(root, lancet_protocol::ImpactKind::WorkspaceMutation);

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        let err = client
            .call::<Create>(CreateParams {
                path: "must-not-land.txt".into(),
                content: "denied content".into(),
                create_parents: false,
            })
            .await
            .expect_err("denied WorkspaceMutation must reject mutate.create");
        assert_pre_dispatch_denial(&err);
        assert!(
            !dir.path().join("must-not-land.txt").exists(),
            "pre-dispatch denial must never create the target file"
        );

        let row = audit.row_by_audit_id(1).expect("denied audit row");
        assert_eq!(row.method, Create::NAME);
        let result = row.result_or_error.as_deref().unwrap_or("");
        assert!(
            !result.is_empty() && !result.to_lowercase().contains("success"),
            "denied call must record a non-success audit result: {result:?}"
        );

        drop(client);
        server_task.abort();
    }

    /// #71: `mutate.edit` declares the same `WorkspaceMutation` kind, so the
    /// same deny must stop it before the backend. The pre-existing file keeps
    /// its byte content — a denied edit is not a partial edit.
    #[tokio::test]
    async fn denied_effect_kind_blocks_mutate_edit_before_backend() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("keep-me.txt");
        std::fs::write(&target, "original content\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let config =
            config_with_denied_effect_kind(root, lancet_protocol::ImpactKind::WorkspaceMutation);

        let (client, server) = CepTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        // The `expected_hash` must be the *correct* current revision. A bogus
        // hash would fail the Mutation Precondition inside the handler and
        // pre-empt the very write this test rules out, making the
        // unchanged-bytes assertion vacuous: it would hold with the
        // pre-dispatch gate removed too.
        let err = client
            .call::<Edit>(EditParams {
                path: "keep-me.txt".into(),
                expected_hash: crate::edit::content_hash(b"original content\n"),
                edits: vec![EditOperation::ReplaceText {
                    old: "original".into(),
                    new: "rewritten".into(),
                    occurrence: None,
                }],
            })
            .await
            .expect_err("denied WorkspaceMutation must reject mutate.edit");
        assert_pre_dispatch_denial(&err);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "original content\n",
            "pre-dispatch denial must not modify the target file"
        );

        drop(client);
        server_task.abort();
    }

    /// #71 acceptance: `deny = ["workspace_observation"]` rejects
    /// `observe.workspace` before the handler, so no workspace read happens.
    #[tokio::test]
    async fn denied_effect_kind_blocks_observe_workspace_before_backend() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("visible.txt"), "content").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let config =
            config_with_denied_effect_kind(root, lancet_protocol::ImpactKind::WorkspaceObservation);

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        let err = client
            .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "visible.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }))
            .await
            .expect_err("denied WorkspaceObservation must reject observe.workspace");
        assert_pre_dispatch_denial(&err);

        let row = audit.row_by_audit_id(1).expect("denied audit row");
        assert_eq!(row.method, ObserveWorkspace::NAME);
        let result = row.result_or_error.as_deref().unwrap_or("");
        assert!(
            !result.is_empty() && !result.to_lowercase().contains("success"),
            "denied observation must record a non-success audit result: {result:?}"
        );
        // #71: the denial short-circuits before any handler, so transport is
        // the only place that can stamp rejection evidence. Without it the row
        // that proves the read was prevented carries no rejection stage or
        // capability, and neither `AuditQueryFilters::rejection_stage` nor
        // `lattice audit --capability` can find it.
        let metadata = row.metadata_json.as_deref().unwrap_or("");
        let metadata: serde_json::Value =
            serde_json::from_str(metadata).expect("denied row must carry metadata json");
        assert_eq!(metadata["rejection_stage"], "policy_gate");
        assert_eq!(metadata["denial_source"], "effect_kind_deny");
        assert_eq!(metadata["required_capability"], "inspect");
        assert_eq!(metadata["policy_enforced"], true);
        // The read never happened, so the handler's own observation evidence
        // must be absent. Asserting the error text carries no file bytes would
        // be vacuous — `CepError` messages are a fixed template that can never
        // hold content — whereas these keys are exactly what a completed
        // `read_file` records (see `handlers::observation`).
        for key in [
            "observation_source",
            "bytes_read",
            "content_hash",
            "visibility_evidence",
        ] {
            assert!(
                metadata.get(key).is_none(),
                "denied observation must not record handler read evidence ({key}): {metadata}"
            );
        }

        drop(client);
        server_task.abort();
    }

    /// #71: a deny set that names a kind the method does not declare must not
    /// pre-empt the call. This is the negative control that keeps the
    /// pre-dispatch check from degenerating into "any deny blocks everything"
    /// and keeps the post-dispatch layer reachable at all.
    #[tokio::test]
    async fn unrelated_denied_effect_kind_does_not_block_mutate_create() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let config =
            config_with_denied_effect_kind(root, lancet_protocol::ImpactKind::SessionStateMutation);

        let (client, server) = CepTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        client
            .call::<Create>(CreateParams {
                path: "allowed.txt".into(),
                content: "written".into(),
                create_parents: false,
            })
            .await
            .expect("mutate.create must survive an unrelated effect-kind deny");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("allowed.txt")).unwrap(),
            "written"
        );

        drop(client);
        server_task.abort();
    }

    // The backend_safety_net test was removed: the safety net was an
    // envelope-era concept that is no longer wired.
}
