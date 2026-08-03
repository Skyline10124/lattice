// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! #59 (Codeberg #37) acceptance tests: production Turn operates directly
//! on the Local Workspace through the closed five-method catalog.
//!
//! These tests exercise the real production runtime
//! (`lancet_api::LancetRuntime::build` with a temp Local root, no Upper, no
//! workspace override, no KVM/MSB) and assert each acceptance criterion from
//! the #59 ticket against the canonical project root on disk:
//!
//! - observe reads current Local state and sees external modifications made
//!   between Tool calls.
//! - mutate.create rejects a pre-existing target with `ReplanRequired`.
//! - mutate.edit checks the expected-hash precondition and atomically installs
//!   the result; a stale hash leaves the target unchanged.
//! - a successful mutation is NOT rolled back when a later step fails.
//! - the closed five-method catalog is exactly what the runtime exposes.

use lancet_api::{LancetRuntime, LancetRuntimeConfig};
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_catalog::catalog;
use lancet_cep::ToolExposure;
use lancet_protocol::Retriability;
use lancet_protocol::evidence::{MutationInstallation, MutationPrecondition, ObservedTarget};
use tempfile::TempDir;

/// The migrated Component Tool's method name (#29). Not a catalog constant: its
/// descriptor authority is `tools/todo-write/manifest.toml`.
const TODO_WRITE: &str = "session.todo_write";

/// The second migrated Component Tool (#30). Same reason it is a literal: its
/// contract lives in `tools/observe-workspace/manifest.toml`, and the
/// `lancet-catalog` types that used to describe it are gone.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

/// The two embedded Component Tools of the default distribution, in the order
/// their source directories sort.
const MUTATE_CREATE: &str = "mutate.create";
const MUTATE_EDIT: &str = "mutate.edit";
const COMPONENT_TOOLS: [&str; 4] = [OBSERVE_WORKSPACE, MUTATE_CREATE, MUTATE_EDIT, TODO_WRITE];

/// Build a production runtime rooted at `root` with all five shipped Tools
/// enabled — the one native catalog method plus four embedded Components —
/// and an empty write-deny list.
fn direct_local_runtime(root: &std::path::Path) -> LancetRuntime {
    let mut policy = PolicyConfig::defaults_for_project_root(root.to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let mut enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    enabled.extend(COMPONENT_TOOLS.iter().map(|name| (*name).to_string()));
    LancetRuntime::build(LancetRuntimeConfig {
        workspace_history: lancet_api::WorkspaceHistoryBacking::Disabled,
        project_root: root.to_path_buf(),
        enabled_tools: enabled,
        policy,
        // The embedded Component Tools are admitted too: `enabled_tools` is the
        // stage-1 capability grant, and the shipped catalog is no longer
        // native-only (#29). Since #30 that grant is what makes *observation*
        // reachable at all — Cell admission is fail-closed and admits nothing by
        // default, so an omitted `observe.workspace` here would fail every read
        // in this file with `capability_not_granted` rather than a policy answer.
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::In,
        },
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: lancet_api::AuditBacking::InMemory,
        model_id: "test".into(),
        runtime_version: "test".into(),
    })
    .expect("direct-local runtime builds")
}

/// The runtime exposes exactly the closed five-method Agent-visible catalog.
#[tokio::test]
async fn direct_local_runtime_exposes_closed_five_method_catalog() {
    let dir = TempDir::new().unwrap();
    let runtime = direct_local_runtime(dir.path());
    let names: Vec<String> = runtime
        .tool_definitions()
        .iter()
        .map(|t| t.name.clone())
        .collect();
    let mut expected = vec![
        "observe.workspace".to_string(),
        "mutate.create".to_string(),
        "mutate.edit".to_string(),
        "fetch.web".to_string(),
        "session.todo_write".to_string(),
    ];
    expected.sort();
    let mut got = names.clone();
    got.sort();
    assert_eq!(got, expected, "Agent-visible catalog must be exactly five");

    // One of the five comes from the native catalog; the other four are the
    // embedded Components, projected from their generated descriptors. The split
    // moved with #31 — the Agent-visible five did not, which is the point: a
    // migration is invisible from the model's side of the boundary.
    let native: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let exposure = ToolExposure::from_catalog_enabled(&native).expect("exposure");
    assert_eq!(exposure.definitions().len(), 1);
    for migrated in COMPONENT_TOOLS {
        assert!(
            !native.contains(&migrated.to_string()),
            "the migrated Tool must not be a native catalog method: {migrated}"
        );
    }

    // The Component's LLM projection carries its manifest description and schema.
    let todo = runtime
        .tool_definitions()
        .iter()
        .find(|t| t.name == TODO_WRITE)
        .expect("the embedded Component is exposed to the Agent");
    assert!(
        todo.description.contains("todo list"),
        "the projection must carry the manifest description: {}",
        todo.description
    );
    assert_eq!(
        todo.parameters["properties"]["todos"]["type"],
        serde_json::json!("array")
    );

    // Same for the observation Tool since #30. Its params schema is the `oneOf`
    // over four mode branches from `tools/observe-workspace/manifest.toml` — the
    // shape a catalog type used to carry, now proving the manifest is the sole
    // descriptor authority.
    let observe = runtime
        .tool_definitions()
        .iter()
        .find(|t| t.name == OBSERVE_WORKSPACE)
        .expect("the migrated observation Tool is exposed to the Agent");
    assert!(
        observe.description.contains("content_hash"),
        "the projection must carry the manifest description: {}",
        observe.description
    );
    assert_eq!(
        observe.parameters["oneOf"].as_array().map(Vec::len),
        Some(4),
        "the four observation modes must survive projection: {}",
        observe.parameters
    );
}

/// observe.workspace read_file sees the current Local state, including an
/// external modification made between two reads.
#[tokio::test]
async fn observe_sees_external_modification_between_tool_calls() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("watch.txt"), "v1\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    // Called untyped since #30: the contract is the Source Tool manifest, not a
    // `lancet-catalog` Rust type, so this is exactly the call a production
    // caller makes.
    let read_watch = serde_json::json!({ "mode": "read_file", "path": "watch.txt" });

    let first = client
        .call_raw(OBSERVE_WORKSPACE, read_watch.clone())
        .await
        .expect("first read");
    assert_eq!(first["mode"], "read_file");
    let first_content = first["result"]["content"]
        .as_str()
        .expect("read_file returns content");
    assert!(first_content.contains("v1"));

    // External writer (the user's editor) changes the file on disk between
    // Tool calls. Direct-Local must observe the new state on the next read.
    std::fs::write(dir.path().join("watch.txt"), "v2-external\n").unwrap();

    let second = client
        .call_raw(OBSERVE_WORKSPACE, read_watch)
        .await
        .expect("second read");
    assert_eq!(second["mode"], "read_file");
    let second_content = second["result"]["content"]
        .as_str()
        .expect("read_file returns content");
    assert!(
        second_content.contains("v2-external"),
        "second read must see the external modification: {second_content}"
    );
}

/// observe.workspace list_directory sees a file added by an external writer
/// between two listings. Guards list/grep/find against stale snapshots.
#[tokio::test]
async fn observe_list_sees_external_addition_between_tool_calls() {
    /// Entry names of a `list_directory` result, in wire order.
    fn entry_names(result: &serde_json::Value) -> Vec<&str> {
        result["result"]["entries"]
            .as_array()
            .expect("list_directory returns entries")
            .iter()
            .map(|entry| entry["name"].as_str().expect("entry name is a string"))
            .collect()
    }

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("orig.txt"), "x\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let list_root = serde_json::json!({ "mode": "list_directory", "path": "." });

    let first = client
        .call_raw(OBSERVE_WORKSPACE, list_root.clone())
        .await
        .expect("first list");
    assert_eq!(first["mode"], "list_directory");
    let first_names = entry_names(&first);
    assert!(first_names.contains(&"orig.txt"));
    assert!(!first_names.contains(&"added.txt"));

    std::fs::write(dir.path().join("added.txt"), "new\n").unwrap();

    let second = client
        .call_raw(OBSERVE_WORKSPACE, list_root)
        .await
        .expect("second list");
    assert_eq!(second["mode"], "list_directory");
    let second_names = entry_names(&second);
    assert!(
        second_names.contains(&"added.txt"),
        "second list must see the externally added file: {second_names:?}"
    );
}

/// Policy denial: a write-deny pattern blocks mutate.create even on the
/// Direct-Local path. Authority policy + Guard L1 + effect checks still
/// cover all mutations.
#[tokio::test]
async fn policy_deny_blocks_create_on_direct_local() {
    let dir = TempDir::new().unwrap();
    let mut policy = PolicyConfig::defaults_for_project_root(dir.path().to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: vec!["secret/**".into()],
        ..Default::default()
    };
    let mut enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    enabled.push(TODO_WRITE.to_string());
    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        workspace_history: lancet_api::WorkspaceHistoryBacking::Disabled,
        project_root: dir.path().to_path_buf(),
        enabled_tools: enabled,
        policy,
        // The embedded Component Tool is admitted too: `enabled_tools` is the
        // stage-1 capability grant, and the shipped catalog is no longer
        // native-only (#29).
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: std::iter::once(TODO_WRITE.to_string()).collect(),
            ..Default::default()
        },
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: lancet_api::AuditBacking::InMemory,
        model_id: "test".into(),
        runtime_version: "test".into(),
    })
    .expect("runtime");
    let client = runtime.client();

    let err = client
        .call_raw(
            "mutate.create",
            serde_json::json!({"path": "secret/leak.txt", "content": "x", "create_parents": true}),
        )
        .await
        .expect_err("denied write must fail");
    // The denied-write pattern is enforced by the guard / effect region
    // check, surfacing as a guard denial or replan — never a silent install.
    assert!(
        matches!(
            err.retriability,
            Retriability::Permanent | Retriability::RequiresReplan
        ),
        "denied write must be rejected, got: {err:?}"
    );
    assert!(
        !dir.path().join("secret/leak.txt").exists(),
        "denied create must not install the file"
    );
}

/// mutate.create rejects a target that already exists on Local with
/// `ReplanRequired` (the no-replace precondition, ADR 0003 decision 7+8).
#[tokio::test]
async fn create_rejects_existing_target_with_replan_required() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("exists.txt"), "first\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let err = client
        .call_raw(
            "mutate.create",
            serde_json::json!({"path": "exists.txt", "content": "second", "create_parents": false}),
        )
        .await
        .expect_err("create on existing target must fail");
    // ADR 0003 decision 7: a stale create precondition (target exists) maps
    // to ReplanRequired, not a permanent execution failure.
    assert_eq!(
        err.retriability,
        Retriability::RequiresReplan,
        "existing create target must map to RequiresReplan, got: {:?}",
        err.retriability
    );
    assert!(
        err.message.contains("already exists"),
        "expected already-exists failure, got: {err:?}"
    );
    // The original file is untouched.
    let on_disk = std::fs::read(dir.path().join("exists.txt")).unwrap();
    assert_eq!(on_disk, b"first\n");
}

/// mutate.create install is no-replace: a file created by a concurrent
/// writer between preflight and install is NOT silently overwritten — the
/// install surfaces the race and the Agent must replan. (ADR 0003 decision 8
/// + decision 9's accepted lost-update window, but the install itself must
/// be no-replace so the Agent's own write does not clobber.)
#[tokio::test]
async fn create_install_is_no_replace_not_overwrite() {
    let dir = TempDir::new().unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    // Create succeeds on an absent target.
    client
        .call_raw(
            "mutate.create",
            serde_json::json!({"path": "race.txt", "content": "agent\n", "create_parents": false}),
        )
        .await
        .expect("first create on absent target");
    assert_eq!(
        std::fs::read(dir.path().join("race.txt")).unwrap(),
        b"agent\n"
    );

    // A second create on the now-existing target must fail with
    // ReplanRequired (no-replace install), leaving the first content intact.
    let err = client
        .call_raw("mutate.create", serde_json::json!({"path": "race.txt", "content": "attacker\n", "create_parents": false}))
        .await
        .expect_err("second create must not overwrite");
    assert_eq!(err.retriability, Retriability::RequiresReplan);
    assert_eq!(
        std::fs::read(dir.path().join("race.txt")).unwrap(),
        b"agent\n",
        "no-replace install must not clobber the existing target"
    );
}

/// mutate.edit with a stale expected_hash leaves the target unchanged and
/// returns ReplanRequired.
#[tokio::test]
async fn edit_stale_hash_leaves_target_unchanged_and_replans() {
    let dir = TempDir::new().unwrap();
    let body = "line one\nline two\n";
    std::fs::write(dir.path().join("edit.txt"), body).unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    // A deliberately wrong hash (the Local Revision has drifted).
    let stale_hash = "blake3:0000000000000000000000000000000000000000000000000000000000000000";
    let err = client
        .call_raw("mutate.edit", serde_json::json!({"path": "edit.txt", "expected_hash": stale_hash, "edits": [{"op": "replace_text", "old": "line one", "new": "line ONE"}]}))
        .await
        .expect_err("stale hash must replan");
    assert_eq!(
        err.retriability,
        Retriability::RequiresReplan,
        "stale expected_hash must map to RequiresReplan, got: {:?}",
        err.retriability
    );
    assert!(
        err.message.contains("stale") || err.message.contains("expected_hash"),
        "expected replan reason, got: {}",
        err.message
    );
    // Target unchanged.
    let on_disk = std::fs::read(dir.path().join("edit.txt")).unwrap();
    assert_eq!(on_disk, body.as_bytes());
}

/// Guest-owned semantic diagnostics re-enter the host so their pre-migration
/// structured CEP classes survive the Component boundary.
#[tokio::test]
async fn edit_semantic_diagnostics_preserve_structured_codes() {
    let dir = TempDir::new().unwrap();
    let body = "same\nsame\n";
    std::fs::write(dir.path().join("diag.txt"), body).unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();
    let expected = format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex());

    let ambiguous = client
        .call_raw("mutate.edit", serde_json::json!({"path": "diag.txt", "expected_hash": expected, "edits": [{"op": "replace_text", "old": "same", "new": "other"}]}))
        .await
        .expect_err("ambiguous edit must replan");
    assert_eq!(
        ambiguous.code,
        lancet_protocol::CepErrorCode::ReplanRequired
    );

    let no_op = client
        .call_raw("mutate.edit", serde_json::json!({"path": "diag.txt", "expected_hash": expected, "edits": [{"op": "replace_text", "old": "same", "new": "same", "occurrence": 1}]}))
        .await
        .expect_err("no-op edit must remain a validation error");
    assert_eq!(no_op.code, lancet_protocol::CepErrorCode::SchemaMismatch);

    let oversized_occurrence = client
        .call_raw("mutate.edit", serde_json::json!({"path": "diag.txt", "expected_hash": expected, "edits": [{"op": "replace_text", "old": "same", "new": "other", "occurrence": 4294967297_u64}]}))
        .await
        .expect_err("occurrence outside the wasm32 index range must fail schema validation");
    assert_eq!(
        oversized_occurrence.code,
        lancet_protocol::CepErrorCode::SchemaMismatch
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("diag.txt")).unwrap(),
        body
    );
}

/// mutate.edit with the correct expected_hash atomically installs the result
/// on Local.
#[tokio::test]
async fn edit_correct_hash_atomically_installs() {
    let dir = TempDir::new().unwrap();
    let body = "hello world\n";
    std::fs::write(dir.path().join("ok.txt"), body).unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let expected = format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex());
    let result = client
        .call_raw("mutate.edit", serde_json::json!({"path": "ok.txt", "expected_hash": expected, "edits": [{"op": "replace_text", "old": "hello", "new": "goodbye"}]}))
        .await
        .expect("edit installs");
    assert_eq!(
        result["changed_files"]
            .as_array()
            .expect("changed_files")
            .len(),
        1
    );

    let on_disk = std::fs::read(dir.path().join("ok.txt")).unwrap();
    assert_eq!(on_disk, b"goodbye world\n");
}

/// A successful Atomic Local Mutation is NOT rolled back when a later step
/// fails. We drive a successful create, then send a stale-hash edit that
/// fails; the created file must remain on disk.
#[tokio::test]
async fn successful_mutation_survives_later_failure() {
    let dir = TempDir::new().unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    // 1. Successful create.
    client
        .call_raw("mutate.create", serde_json::json!({"path": "survives.txt", "content": "persisted\n", "create_parents": false}))
        .await
        .expect("create");
    assert!(dir.path().join("survives.txt").exists());

    // 2. A later step fails (stale-hash edit on a different file).
    std::fs::write(dir.path().join("other.txt"), "x\n").unwrap();
    let stale = "blake3:0000000000000000000000000000000000000000000000000000000000000000";
    let _ = client
        .call_raw("mutate.edit", serde_json::json!({"path": "other.txt", "expected_hash": stale, "edits": [{"op": "replace_text", "old": "x", "new": "y"}]}))
        .await;

    // The earlier successful create is still on disk — Direct-Local does not
    // pretend to roll back mutations when a later Tool call fails.
    let on_disk = std::fs::read(dir.path().join("survives.txt")).unwrap();
    assert_eq!(on_disk, b"persisted\n");
}

#[tokio::test]
async fn cancelled_component_mutation_before_entry_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();
    let (call, control) = client.start_call_raw(
        MUTATE_CREATE.to_string(),
        serde_json::json!({
            "path": "cancelled.txt",
            "content": "must-not-land\n",
            "create_parents": false
        }),
    );
    control.cancel();

    let err = call
        .await
        .expect_err("a pre-cancelled mutation must not enter the Component");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::CallCancelled);
    assert!(!dir.path().join("cancelled.txt").exists());
}

#[tokio::test]
async fn component_mutation_install_failure_leaves_no_partial_target() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    // Root and some unusual filesystems bypass the permission failure this
    // product-level test needs. Detect that directly instead of assuming an
    // effective uid, then leave the deterministic fault to Host unit tests.
    let probe = locked.join(".permission-probe");
    if std::fs::write(&probe, b"").is_ok() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_file(probe);
        eprintln!("skipping read-only install fault: this process can write mode 0555 dirs");
        return;
    }

    let runtime = direct_local_runtime(dir.path());
    let err = runtime
        .client()
        .call_raw(
            MUTATE_CREATE,
            serde_json::json!({
                "path": "locked/partial.txt",
                "content": "must-not-land\n",
                "create_parents": false
            }),
        )
        .await
        .expect_err("read-only parent must fail the Component install");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(err.code, lancet_protocol::CepErrorCode::ExecutionFailed);
    assert!(!locked.join("partial.txt").exists());
    assert!(
        std::fs::read_dir(&locked).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".lattice-tmp-")),
        "failed Component install must leave neither target nor temp residue"
    );
}

/// Audit records method-local evidence (guarded path + Local Revision) and
/// carries no routing identity: no `execution_backend`, no
/// `upper_workspace_id`, no adapter-preflight or generic routing invocation
/// flags (#60 (Codeberg #38)).
///
/// Both migrated Component paths get a leg: mutation and observation record
/// their concrete targets in the versioned evidence tables and leave only an
/// evidence pointer in the audit row. The no-routing-identity assertion is the
/// same for both — that is the #60 subject.
#[tokio::test]
async fn audit_records_method_local_evidence_without_routing_metadata() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "audit-body\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    client
        .call_raw("mutate.create", serde_json::json!({"path": "b.txt", "content": "audit-body\n", "create_parents": false}))
        .await
        .expect("component create");
    client
        .call_raw(
            OBSERVE_WORKSPACE,
            serde_json::json!({ "mode": "read_file", "path": "a.txt" }),
        )
        .await
        .expect("component read");
    runtime.audit().await_pending().await;

    let rows = runtime
        .audit()
        .query_rows(&lancet_native::AuditQueryFilters::default())
        .expect("rows");
    assert_eq!(rows.len(), 2, "audit must record both calls: {rows:?}");

    // ── The mutate.create Component leg: Cell evidence, not native metadata ──
    let create_row = rows
        .iter()
        .find(|row| row.method == "mutate.create")
        .expect("the create call is audited");
    let create_meta = create_row.metadata_json.as_deref().unwrap_or("{}");
    assert!(
        create_meta.contains("evidence_id") || create_meta.contains("component_cells"),
        "Component mutate.create must point at Cell evidence: {create_meta}"
    );
    assert!(
        create_meta.contains("mutate:write:workspace")
            || create_meta.contains("alloc:write:workspace"),
        "Component mutate.create must record mutation/result cells: {create_meta}"
    );
    assert!(
        dir.path().join("b.txt").is_file(),
        "mutate.create Component must have installed the file on disk"
    );
    let create_evidence_id = serde_json::from_str::<serde_json::Value>(create_meta)
        .expect("create metadata json")["evidence_id"]
        .as_i64()
        .expect("the create Component row points at its evidence");
    let create_evidence = runtime
        .audit()
        .call_evidence(lancet_native::EvidenceId(create_evidence_id))
        .expect("create evidence read")
        .expect("create evidence row exists");
    assert_eq!(create_evidence.tool, MUTATE_CREATE);
    let create_effect = create_evidence
        .effects
        .effects
        .iter()
        .find(|effect| matches!(&effect.target, ObservedTarget::Path { path } if path == "b.txt"))
        .expect("create evidence names the resolved target");
    assert_eq!(create_effect.context_measure, None);
    let mutation = create_effect
        .mutation
        .as_ref()
        .expect("create effect carries MutationEvidence");
    assert_eq!(mutation.before_revision, None);
    assert!(
        mutation
            .after_revision
            .as_deref()
            .is_some_and(|revision| revision.starts_with("blake3:")),
        "create after revision must be host-computed: {mutation:?}"
    );
    assert_eq!(mutation.precondition, MutationPrecondition::Satisfied);
    assert_eq!(mutation.installation, MutationInstallation::Installed);

    // ── The Component leg: the same fact, one indirection away ───────────
    let observe_row = rows
        .iter()
        .find(|row| row.method == OBSERVE_WORKSPACE)
        .expect("the observation is audited");
    assert_eq!(
        observe_row.semantic_stage, "cell_component",
        "since #30 the observation is labeled by the vocabulary that authorized it"
    );
    let observe_meta = observe_row.metadata_json.as_deref().unwrap_or("{}");
    let evidence_id = serde_json::from_str::<serde_json::Value>(observe_meta)
        .expect("metadata json")["evidence_id"]
        .as_i64()
        .expect("a Component row points at its evidence");
    let evidence = runtime
        .audit()
        .call_evidence(lancet_native::EvidenceId(evidence_id))
        .expect("evidence read")
        .expect("evidence row exists");
    assert_eq!(evidence.tool, OBSERVE_WORKSPACE);
    // The concrete path the host resolved and authorized — the Component's
    // equivalent of `guarded_path`, recorded by the host rather than reported by
    // the guest.
    assert!(
        evidence.effects.effects.iter().any(|effect| matches!(
            &effect.target,
            ObservedTarget::Path { path } if path == "a.txt"
        )),
        "the observation's evidence must name the resolved Local path: {:?}",
        evidence.effects.effects
    );

    // No routing identity or invocation flags survive, in either vocabulary.
    for meta in [create_meta, observe_meta] {
        for retired in [
            "execution_backend",
            "upper_workspace_id",
            "adapter_preflight_invoked",
            "backend_invoked",
            "direct-local",
        ] {
            assert!(
                !meta.contains(retired),
                "audit metadata must not carry retired routing field '{retired}': {meta}"
            );
        }
    }
}
