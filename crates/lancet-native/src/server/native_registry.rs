// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native catalog method registration.

use crate::handlers::register_external_methods;
use crate::server::registry::MethodRegistry;
use lancet_protocol::CepError;

/// Register the remaining LANCET native handlers.
///
/// `session.todo_write` (#29), `observe.workspace` (#30), and
/// `mutate.create` / `mutate.edit` (#31) are deliberately absent: each is an
/// embedded Component Tool, and each migration deleted its native handler rather
/// than keeping it as a fallback. The absence is the design — a runtime whose
/// composition does not embed the Component has no other way to serve the
/// method, and dispatch reports `MethodNotFound` instead of quietly falling
/// back to a second implementation with different enforcement.
pub fn register_cep_native_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_native_external(registry)?;
    Ok(())
}

/// Register native CEP `fetch.web` handler.
pub fn register_native_external(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_external_methods(registry)
}

pub fn register_production_method_registry(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_cep_native_methods(registry)
}

#[cfg(test)]
mod cep_native_registry_tests {
    use super::*;
    use crate::{CepServerNativeExt, CepTransport, ServerRunConfig};
    use lancet_authority::policy::CepTurnContext;
    use lancet_catalog::CepMethod;
    use lancet_catalog::catalog;
    use lancet_catalog::methods::Fetch;
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

    fn assert_pre_dispatch_denial(err: &lancet_protocol::CepError) {
        assert_eq!(err.code, CepErrorCode::EffectViolation);
        assert!(
            err.message.contains("declared effect denied"),
            "pre-dispatch denial must be the declared-kind layer, got: {}",
            err.message
        );
    }

    /// Issue #46 / #71: pre-dispatch effect-kind denial on the remaining native
    /// method (`fetch.web`) must never call the backend. Mutation methods moved
    /// to Components in #31 and are no longer covered by this catalog-keyed
    /// check — their authority is Cell admission + the `workspace-mutate` host.
    #[tokio::test]
    async fn denied_effect_kind_blocks_fetch_web_before_backend() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let config = config_with_denied_effect_kind(
            root,
            lancet_protocol::ImpactKind::ExternalObservation,
        );

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        let err = client
            .call_raw(
                Fetch::NAME,
                serde_json::json!({ "url": "https://example.com/" }),
            )
            .await
            .expect_err("denied ExternalObservation must reject fetch.web");
        assert_pre_dispatch_denial(&err);

        let row = audit.row_by_audit_id(1).expect("denied audit row");
        assert_eq!(row.method, Fetch::NAME);
        let result = row.result_or_error.as_deref().unwrap_or("");
        assert!(
            !result.is_empty() && !result.to_lowercase().contains("success"),
            "denied call must record a non-success audit result: {result:?}"
        );
        let metadata = row.metadata_json.as_deref().unwrap_or("");
        let metadata: serde_json::Value =
            serde_json::from_str(metadata).expect("denied row must carry metadata json");
        assert_eq!(metadata["rejection_stage"], "policy_gate");
        assert_eq!(metadata["denial_source"], "effect_kind_deny");
        assert_eq!(metadata["policy_enforced"], true);

        drop(client);
        server_task.abort();
    }

    /// Unrelated effect-kind deny must not block the remaining native method.
    #[tokio::test]
    async fn unrelated_effect_kind_deny_does_not_block_fetch_web() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        // Deny WorkspaceMutation — fetch.web must still be reachable through
        // the native registry (it will fail later on network policy / host
        // allowlist, not on this declared-kind check).
        let config =
            config_with_denied_effect_kind(root, lancet_protocol::ImpactKind::WorkspaceMutation);

        let (client, server) = CepTransport::pair(8);
        let server_task = tokio::spawn(async move {
            server.run_native(registry, config).await.unwrap();
        });

        let value = client
            .call_raw(
                Fetch::NAME,
                serde_json::json!({ "url": "https://example.com/" }),
            )
            .await
            .expect("fetch.web must survive an unrelated WorkspaceMutation effect-kind deny");
        assert_eq!(value["status"], 200);

        drop(client);
        server_task.abort();
    }

    #[test]
    fn production_registry_does_not_register_migrated_mutation_methods() {
        let registry = production_registry();
        for migrated in ["mutate.create", "mutate.edit", "observe.workspace", "session.todo_write"]
        {
            assert!(
                !registry.contains(migrated),
                "{migrated} must not remain a native registry method"
            );
        }
        assert!(registry.contains(Fetch::NAME));
    }
}
