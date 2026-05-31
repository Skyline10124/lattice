//! VER-23 / LEG-02 cep_mode integration tests.
//!
//! **LEG-02 invariant:** `cep_mode = Off` does **NOT** re-enable legacy direct tool calls.
//! In the production stack (post-API), there is no legacy tool executor path.
//! `Off` mode only skips the effect-region check; the CEP policy hook still runs and will
//! deny requests that lack the required capability declaration.
//!
//! ## Modes tested
//!
//! | Mode    | Effect overreach | Policy deny (no capability) |
//! |---------|------------------|-----------------------------|
//! | Enforce | Err(EffectViolation) | Err(MissingCapability) |
//! | Warn    | Ok; audit row has `effect_warning` in metadata | Err(MissingCapability) |
//! | Off     | Ok (effect check skipped) | Err(MissingCapability) — policy still runs |
//!
//! ## Approach
//!
//! Enforce/Warn/Off tests use a **custom overreach handler** (not the production
//! registry) to produce a deterministic out-of-scope effect for the effect checker.
//! The policy-deny-still-works tests use the production registry to demonstrate that
//! the policy hook runs regardless of `cep_mode`.

use std::path::PathBuf;
use std::sync::Arc;

use cep_executor::{
    register_production_method_registry, CepAuditStore, CepTransport, MethodRegistry,
    ServerRunConfig,
};
use cep_protocol::envelope::{BaseExecutionEnvelope, CepMode, ImpactKind};
use cep_protocol::methods::session::{TodoWrite, TodoWriteParams, TodoWriteResult};
use cep_protocol::CepErrorCode;
use lattice_profile::CepSection;
use tempfile::TempDir;

// ── Shared overreach handler ───────────────────────────────────────────
//
// A custom `session.todo_write` handler that:
// 1. Sets an EMPTY authorized region (scope: nothing allowed).
// 2. Records a `WorkspaceMutation` effect at a path outside the empty region.
// 3. Returns Ok — so the effect checker is the only thing that can catch the overreach.
//
// Base envelope grants `MutateSessionState` (base source is allowed for session_state),
// so the policy check passes. The overreach is in the effect region check.

fn overreach_todo_handler() -> MethodRegistry {
    use cep_executor::effect::{
        ledger::{EffectSource, ObservedEffect, ObservedScope},
        region::AuthorizedEffectRegion,
    };
    use cep_protocol::method::RequiredCapability;

    let mut registry = MethodRegistry::new();
    registry
        .register_method::<TodoWrite, _, _>(|params: TodoWriteParams, ctx| async move {
            // Intentionally set an empty authorized region (no paths allowed).
            ctx.authorized_region.set(AuthorizedEffectRegion::empty());
            // Record a WorkspaceMutation effect OUTSIDE the empty authorized region.
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/out/of/scope/overreach")),
                required_capability: RequiredCapability::WritePath,
                source: EffectSource::HandlerBackend,
            });
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        })
        .expect("overreach handler registration");
    registry
}

/// Build a transport pair with the overreach registry and the given `CepMode`.
fn overreach_server_config(cep_mode: CepMode) -> (ServerRunConfig, Arc<CepAuditStore>) {
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.cep_mode = cep_mode;
    let audit = Arc::clone(&config.audit);
    (config, audit)
}

// ── Enforce test ───────────────────────────────────────────────────────

/// Enforce mode: effect overreach → `EffectViolation` error (not Ok with warning).
#[tokio::test]
async fn cep_mode_enforce_effect_overreach_denied() {
    let (config, audit) = overreach_server_config(CepMode::Enforce);
    let (client, server) = CepTransport::pair(8);
    let registry = overreach_todo_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let err = client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .expect_err("enforce mode must reject overreach as EffectViolation");

    assert_eq!(
        err.code,
        CepErrorCode::EffectViolation,
        "enforce must produce EffectViolation, got {:?}",
        err.code
    );
    assert!(
        !err.message.is_empty(),
        "EffectViolation must have a message"
    );

    drop(client);
    server_task.abort();

    // Audit row must record the violation.
    let row = audit.row_by_audit_id(1).expect("enforce row");
    let result_or_meta = format!(
        "{}|{}",
        row.result_or_error.as_deref().unwrap_or(""),
        row.metadata_json.as_deref().unwrap_or("")
    );
    assert!(
        result_or_meta.contains("EffectViolation") || result_or_meta.contains("effect_violation"),
        "enforce audit row must record violation, got: {result_or_meta}"
    );
}

// ── Warn test ──────────────────────────────────────────────────────────

/// Warn mode: effect overreach → Ok (transport returns success); durable metadata
/// in the `CepAuditStore` row contains `effect_warning`.
#[tokio::test]
async fn cep_mode_warn_effect_overreach_ok_with_audit_warning() {
    let (config, audit) = overreach_server_config(CepMode::Warn);
    let (client, server) = CepTransport::pair(8);
    let registry = overreach_todo_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    // In Warn mode, the dispatch succeeds even though the effect overreached.
    let result = client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .expect("warn mode must return Ok for overreach");
    assert!(result.todos.is_empty(), "warn result must pass through");

    drop(client);
    server_task.abort();

    // Assert durable warning in CepAuditStore — NOT in CepExecutorAdapter return value.
    let row = audit.row_by_audit_id(1).expect("warn row");
    let metadata = row.metadata_json.as_deref().unwrap_or("");
    assert!(
        metadata.contains("effect_warning"),
        "warn audit row metadata must contain effect_warning (l2 or transport warning), got: {metadata}"
    );
}

// ── Off test ──────────────────────────────────────────────────────────

/// Off mode — Part 1: effect overreach passes (effect check is skipped).
#[tokio::test]
async fn cep_mode_off_effect_overreach_passes() {
    let (config, _audit) = overreach_server_config(CepMode::Off);
    let (client, server) = CepTransport::pair(8);
    let registry = overreach_todo_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    // Off skips the effect-region check — overreach handler succeeds.
    let result = client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .expect("off mode must pass through overreach");
    assert!(result.todos.is_empty());

    drop(client);
    server_task.abort();
}

/// Off mode — Part 2 (LEG-02): policy/capability deny still returns Err when the
/// envelope lacks the required capability.
///
/// Off does NOT re-enable legacy direct tool calls. The CEP policy hook always runs.
/// A `workspace.write_file` call without a declared `WritePath` capability must be
/// denied even in Off mode.
#[tokio::test]
async fn cep_mode_off_policy_deny_still_enforced() {
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();
    let canonical = std::fs::canonicalize(root).expect("canonicalize");
    let canonical_str = canonical.to_str().unwrap();

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    // Base envelope: ReadPath only — no WritePath or RunCommand.
    let base_envelope =
        BaseExecutionEnvelope::for_workspace_root(canonical_str, "off mode policy test");

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.cep_mode = CepMode::Off; // Off: effect check skipped, but policy still runs.
    config.base_envelope = base_envelope;

    // Set up a proper policy hook for the workspace root.
    {
        use cep_executor::policy::default::{DefaultPolicyHook, PolicyConfig};
        let policy_cfg = PolicyConfig::defaults_for_project_root(canonical.clone());
        let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));
        config.policy_hook = hook.clone();
        config.default_policy_hook = Some(hook);
        config.cep_context = Arc::new(cep_executor::CepTurnContext::defaults_for_project_root(
            canonical_str,
        ));
    }

    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });
    tokio::task::yield_now().await;

    // workspace.write_file without WritePath in any envelope → policy denial.
    // Off mode does NOT bypass the policy check.
    use cep_protocol::methods::workspace::{WriteFile, WriteFileParams};
    let err = client
        .call::<WriteFile>(WriteFileParams {
            path: "should_be_denied.txt".into(),
            content: "this should never be written".into(),
        })
        .await
        .expect_err("write_file without WritePath must be denied even in Off mode");

    assert_eq!(
        err.code,
        CepErrorCode::CapabilityNotGranted,
        "off mode policy deny must be CapabilityNotGranted, got {:?}: {}",
        err.code,
        err.message
    );
    assert!(
        err.message.contains("capability")
            || err.message.contains("source")
            || err.message.contains("WritePath"),
        "denial message must reference capability: {}",
        err.message
    );

    // Verify the file was NOT created.
    assert!(
        !root.join("should_be_denied.txt").exists(),
        "policy-denied write must not create the file"
    );

    drop(client);
    server_task.abort();
}

// ── Default enforce test ───────────────────────────────────────────────

/// Profile default: `CepSection::default()` must yield `CepMode::Enforce`.
///
/// Documents the LEG-02 requirement that the effective default mode is Enforce
/// when no `[cep]` section is present in agent.toml.
#[test]
fn cep_mode_default_enforce_from_profile_schema() {
    let section = CepSection::default();
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "CepSection::default() must produce CepMode::Enforce, got {:?}",
        section.mode
    );
}

/// CepSection deserialization: omitting `mode` from `[cep]` table preserves Enforce default.
///
/// Documents the LEG-02 guarantee at the serialization level: the TOML `[cep]` section
/// without a `mode` key deserializes to `CepMode::Enforce`.
#[test]
fn cep_mode_enforce_when_cep_section_mode_absent() {
    // Deserialize a bare [cep] table (no mode field) → Enforce.
    let cep_toml = r#"
# No mode key — must default to Enforce
"#;
    let section: CepSection = toml::from_str(cep_toml).expect("parse CepSection toml");
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "CepSection without mode must default to Enforce, got {:?}",
        section.mode
    );
}
