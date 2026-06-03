//! cep_mode integration tests.
//!
//! Invariant: `cep_mode = Off` does **not** re-enable retired direct tool calls.
//! In the production stack (post-API), there is no legacy tool executor path.
//! `Off` mode only skips the effect-region check; the CEP policy hook still runs and will
//! deny requests that lack the required capability declaration.
//!
//! ## Modes tested
//!
//! | Mode    | Region overreach | Effect-kind deny | Policy deny (no capability) |
//! |---------|------------------|------------------|-----------------------------|
//! | Enforce | Err(EffectViolation) | Err(EffectViolation) | Err(MissingCapability) |
//! | Warn    | Ok; audit row has `effect_warning` in metadata | Err(EffectViolation) | Err(MissingCapability) |
//! | Off     | Ok (region check skipped) | Err(EffectViolation) | Err(MissingCapability) — policy still runs |
//!
//! ## Approach
//!
//! Enforce/Warn/Off tests use a **custom overreach handler** (not the production
//! registry) to produce a deterministic out-of-scope effect for the effect checker.
//! The policy-deny-still-works tests use the production registry to demonstrate that
//! the policy hook runs regardless of `cep_mode`.

use std::path::PathBuf;
use std::sync::Arc;

use lancet_catalog::methods::command::{Run, RunParams, RunResult};
use lancet_catalog::methods::session::{TodoWrite, TodoWriteParams, TodoWriteResult};
use lancet_native::isolation::{
    CommandIsolationLandlockBehavior, CommandIsolationMode, CommandIsolationRuntimeConfig,
    CommandIsolationUnsupportedHostBehavior,
};
use lancet_native::{
    register_production_method_registry, CepAuditStore, CepTransport, MethodRegistry,
    ServerRunConfig,
};
use lancet_protocol::envelope::{BaseExecutionEnvelope, CepMode, ImpactKind};
use lancet_protocol::BoundaryDimension;
use lancet_protocol::CepErrorCode;
use lattice_profile::LancetSection;
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
    use lancet_authority::effect::{
        ledger::{EffectSource, ObservedEffect, ObservedScope},
        region::AuthorizedEffectRegion,
    };
    use lancet_catalog::method::RequiredCapability;

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

fn overreach_server_config_with_denied_effect(
    cep_mode: CepMode,
    denied_effect: ImpactKind,
) -> (ServerRunConfig, Arc<CepAuditStore>) {
    let (mut config, audit) = overreach_server_config(cep_mode);
    let mut policy = config.cep_context.policy.clone();
    policy.denied_effect_kinds.insert(denied_effect);
    config.cep_context = Arc::new(lancet_authority::CepTurnContext::from_policy(policy));
    (config, audit)
}

fn command_composite_handler() -> MethodRegistry {
    use lancet_authority::effect::{
        ledger::{EffectSource, ObservedEffect, ObservedScope},
        region::AuthorizedEffectRegion,
    };
    use lancet_catalog::method::RequiredCapability;

    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Run, _, _>(|_params: RunParams, ctx| async move {
            let mut region = AuthorizedEffectRegion::empty();
            region.kinds.insert(ImpactKind::CommandExecution);
            region.kinds.insert(ImpactKind::WorkspaceMutation);
            region.command_allowed = true;
            region.write_path_prefixes.push(PathBuf::from("/project"));
            ctx.authorized_region.set(region);
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root("/project".to_string()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::HandlerBackend,
            });
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/project/generated.txt")),
                required_capability: RequiredCapability::WritePath,
                source: EffectSource::HandlerBackend,
            });
            Ok(RunResult {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                stdout_truncated: None,
                stderr_truncated: None,
                stdout_bytes: Some(0),
                stderr_bytes: Some(0),
            })
        })
        .expect("command composite handler registration");
    registry
}

fn command_isolation_config_observer_handler() -> MethodRegistry {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Run, _, _>(|_params: RunParams, ctx| async move {
            assert_eq!(ctx.command_isolation.mode, CommandIsolationMode::Enforce);
            assert_eq!(
                ctx.command_isolation.required_boundaries,
                vec![BoundaryDimension::Network, BoundaryDimension::Landlock]
            );
            assert_eq!(
                ctx.command_isolation.optional_boundaries,
                vec![BoundaryDimension::Filesystem]
            );
            assert_eq!(
                ctx.command_isolation.read_only_toolchain_paths,
                vec![PathBuf::from("/usr/bin")]
            );
            assert_eq!(
                ctx.command_isolation.scratch_paths,
                vec![PathBuf::from("build/tmp")]
            );
            assert_eq!(
                ctx.command_isolation.landlock,
                CommandIsolationLandlockBehavior::Auto
            );
            assert_eq!(
                ctx.command_isolation.unsupported_host,
                CommandIsolationUnsupportedHostBehavior::FailClosed
            );
            Ok(RunResult {
                exit_code: 0,
                stdout: "command isolation config visible".into(),
                stderr: String::new(),
                stdout_truncated: None,
                stderr_truncated: None,
                stdout_bytes: Some("command isolation config visible".len()),
                stderr_bytes: Some(0),
            })
        })
        .expect("command isolation observer handler registration");
    registry
}

#[tokio::test]
async fn cep_mode_integration_off_keeps_command_isolation_config_visible_to_handlers() {
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.cep_mode = CepMode::Off;
    config.command_isolation = CommandIsolationRuntimeConfig {
        mode: CommandIsolationMode::Enforce,
        required_boundaries: vec![BoundaryDimension::Network, BoundaryDimension::Landlock],
        optional_boundaries: vec![BoundaryDimension::Filesystem],
        read_only_toolchain_paths: vec![PathBuf::from("/usr/bin")],
        scratch_paths: vec![PathBuf::from("build/tmp")],
        landlock: CommandIsolationLandlockBehavior::Auto,
        unsupported_host: CommandIsolationUnsupportedHostBehavior::FailClosed,
        host_probe: None,
    };

    let (client, server) = CepTransport::pair(8);
    let registry = command_isolation_config_observer_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let result = client
        .call::<Run>(RunParams {
            argv: vec!["echo".into(), "ignored-by-observer".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        })
        .await
        .expect("cep_mode Off must not erase command isolation runtime config");

    assert_eq!(result.stdout, "command isolation config visible");

    drop(client);
    server_task.abort();
}

#[test]
fn cep_mode_integration_missing_command_isolation_defaults_to_off() {
    let config = ServerRunConfig::in_memory_for_tests();
    assert_eq!(config.command_isolation.mode, CommandIsolationMode::Off);
    assert!(
        config.command_isolation.required_boundaries.is_empty(),
        "missing [lancet.isolation.command] must not request physical boundaries"
    );
    assert!(
        config.command_isolation.optional_boundaries.is_empty(),
        "missing [lancet.isolation.command] must not imply optional protection"
    );
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

#[tokio::test]
async fn effect_deny_rejects_in_warn_mode() {
    let (config, _audit) =
        overreach_server_config_with_denied_effect(CepMode::Warn, ImpactKind::WorkspaceMutation);
    let (client, server) = CepTransport::pair(8);
    let registry = overreach_todo_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let err = client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .expect_err("effect-kind deny must hard reject even in Warn mode");

    assert_eq!(err.code, CepErrorCode::EffectViolation);
    assert!(
        err.message.contains("denied by effect-kind policy"),
        "deny error must identify effect-kind policy: {}",
        err.message
    );

    drop(client);
    server_task.abort();
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

#[tokio::test]
async fn effect_deny_rejects_in_off_mode() {
    let (config, _audit) =
        overreach_server_config_with_denied_effect(CepMode::Off, ImpactKind::WorkspaceMutation);
    let (client, server) = CepTransport::pair(8);
    let registry = overreach_todo_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let err = client
        .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
        .await
        .expect_err("effect-kind deny must hard reject even in Off mode");

    assert_eq!(err.code, CepErrorCode::EffectViolation);
    assert!(
        err.message.contains("denied by effect-kind policy"),
        "deny error must identify effect-kind policy: {}",
        err.message
    );

    drop(client);
    server_task.abort();
}

#[tokio::test]
async fn effect_deny_catches_command_composite_workspace_mutation() {
    let (config, _audit) =
        overreach_server_config_with_denied_effect(CepMode::Enforce, ImpactKind::WorkspaceMutation);
    let (client, server) = CepTransport::pair(8);
    let registry = command_composite_handler();
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

    let err = client
        .call::<Run>(RunParams {
            argv: vec!["touch".into(), "generated.txt".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::WorkspaceMutation),
        })
        .await
        .expect_err("deny WorkspaceMutation must catch exec.command_run composite mutation");

    assert_eq!(err.code, CepErrorCode::EffectViolation);
    assert!(
        err.message.contains("denied by effect-kind policy"),
        "deny error must identify effect-kind policy: {}",
        err.message
    );

    drop(client);
    server_task.abort();
}

/// Off mode: policy/capability deny still returns Err when the
/// envelope lacks the required capability.
///
/// Off does NOT re-enable legacy direct tool calls. The CEP policy hook always runs.
/// A `mutate.create` call without a declared `WritePath` capability must be
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
        use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
        use lancet_authority::CepTurnContext;
        let policy_cfg = PolicyConfig::defaults_for_project_root(canonical.clone());
        let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));
        config.policy_hook = hook.clone();
        config.default_policy_hook = Some(hook);
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(canonical_str));
    }

    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });
    tokio::task::yield_now().await;

    // mutate.create without WritePath in any envelope -> policy denial.
    // Off mode does NOT bypass the policy check.
    use lancet_catalog::methods::workspace::{Create, CreateParams};
    let err = client
        .call::<Create>(CreateParams {
            path: "should_be_denied.txt".into(),
            content: "this should never be written".into(),
        })
        .await
        .expect_err("mutate.create without WritePath must be denied even in Off mode");

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

/// Profile default: `LancetSection::default()` must yield `CepMode::Enforce`.
///
/// Documents that the effective default mode is Enforce
/// when no `[lancet]` section is present in agent.toml.
#[test]
fn cep_mode_default_enforce_from_profile_schema() {
    let section = LancetSection::default();
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "LancetSection::default() must produce CepMode::Enforce, got {:?}",
        section.mode
    );
}

/// LancetSection deserialization: omitting `mode` from `[lancet]` table preserves Enforce default.
///
/// Documents the serialization guarantee: the TOML `[lancet]` section
/// without a `mode` key deserializes to `CepMode::Enforce`.
#[test]
fn cep_mode_enforce_when_cep_section_mode_absent() {
    // Deserialize a bare [lancet] table (no mode field) → Enforce.
    let cep_toml = r#"
# No mode key — must default to Enforce
"#;
    let section: LancetSection = toml::from_str(cep_toml).expect("parse LancetSection toml");
    assert_eq!(
        section.mode,
        CepMode::Enforce,
        "LancetSection without mode must default to Enforce, got {:?}",
        section.mode
    );
}
