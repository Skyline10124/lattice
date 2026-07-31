// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native catalog method registration.

use crate::handlers::{register_external_methods, register_mutation_methods};
use crate::server::registry::MethodRegistry;
use lancet_protocol::CepError;

/// Register the remaining LANCET native handlers.
///
/// `session.todo_write` (#29) and `observe.workspace` (#30) are deliberately
/// absent: both are embedded Component Tools now, and each migration deleted its
/// native handler rather than keeping it as a fallback. The absence is the
/// design — a runtime whose composition does not embed the Component has no other
/// way to serve the method, and dispatch reports `MethodNotFound` instead of
/// quietly falling back to a second implementation with different enforcement.
pub fn register_cep_native_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_native_external(registry)?;
    Ok(())
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
    use crate::{CepServerNativeExt, CepTransport, ServerRunConfig};
    use lancet_authority::policy::CepTurnContext;
    use lancet_catalog::CepMethod;
    use lancet_catalog::catalog;
    use lancet_catalog::methods::workspace::{
        Create, CreateParams, Edit, EditOperation, EditParams,
    };
    use lancet_protocol::CepErrorCode;
    use std::sync::Arc;
    use tempfile::TempDir;

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

    // Three `observe.workspace` transport tests lived here and went with the
    // native handler (#30):
    //
    // - `cep_native_registry_native_error_returns_guard_denied` dispatched
    //   `../outside` through the typed registry and asserted `GuardDenied`. Path
    //   containment is host-side now and is covered where it is enforced:
    //   `lancet_runtime::host_workspace::tests::parent_traversal_is_refused_lexically`
    //   and `symlink_escaping_the_root_is_refused`.
    // - `cep_native_registry_labels_native_methods` asserted the row carried
    //   `SEMANTIC_STAGE_NATIVE`. The method is dispatched as a Component now, so
    //   the correct label is `cell_component` — `CallAuditMetadata::for_component_dispatch`
    //   supplies it, and `lattice-api`'s dogfood matrix pins it end to end.
    // - `recursive_visibility_evidence_survives_transport_finalization` asserted
    //   the handler's `visibility_evidence` metadata blob survived finalization.
    //   That blob was the native handler's own shape; a Component call records
    //   the same decisions as versioned Cell evidence instead
    //   (`DeniedOperation.reason` carries the `HiddenReason` audit label), which
    //   `host_workspace::tests::denied_reads_are_refused_and_recorded_with_their_reason`
    //   pins. Its non-leak assertions (no `secret/key`, no sentinel, no root
    //   path in the row) are preserved there and in
    //   `all_four_modes_share_one_visibility_decision`.

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
        // #71: the denial short-circuits before any handler, so transport is the
        // only place that can stamp rejection evidence. Without it the row that
        // proves the write was prevented carries no rejection stage or
        // capability, and neither `AuditQueryFilters::rejection_stage` nor
        // `lattice audit --capability` can find it. This assertion used to live
        // on an `observe.workspace` twin; #30 migrated that method to a Component
        // Tool, whose authority is its admitted Cells and which never reaches the
        // legacy effect-kind check at all, so the mutation case carries it now.
        let metadata = row.metadata_json.as_deref().unwrap_or("");
        let metadata: serde_json::Value =
            serde_json::from_str(metadata).expect("denied row must carry metadata json");
        assert_eq!(metadata["rejection_stage"], "policy_gate");
        assert_eq!(metadata["denial_source"], "effect_kind_deny");
        assert_eq!(metadata["required_capability"], "write_path");
        assert_eq!(metadata["policy_enforced"], true);
        // The handler never ran, so its own completion evidence must be absent.
        for key in ["bytes_written", "created_dirs", "content_hash"] {
            assert!(
                metadata.get(key).is_none(),
                "a denied mutation must not record handler evidence ({key}): {metadata}"
            );
        }

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
