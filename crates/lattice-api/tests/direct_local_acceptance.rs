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

mod support;

use lancet_api::LancetRuntimeConfig;
use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_protocol::evidence::{MutationInstallation, MutationPrecondition, ObservedTarget};
use lancet_protocol::{CepErrorCode, Retriability};
use support::ReservedRuntime;
use tempfile::TempDir;

/// The migrated Component Tool's method name (#29). Not a catalog constant: its
/// descriptor authority is `tools/todo-write/manifest.toml`.
const TODO_WRITE: &str = "session.todo_write";

/// The second migrated Component Tool (#30). Same reason it is a literal: its
/// contract lives solely in `tools/observe-workspace/manifest.toml`.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

/// The five embedded Component Tools of the default distribution.
const MUTATE_CREATE: &str = "mutate.create";
const MUTATE_EDIT: &str = "mutate.edit";
const FETCH_WEB: &str = "fetch.web";
const COMPONENT_TOOLS: [&str; 5] = [
    OBSERVE_WORKSPACE,
    MUTATE_CREATE,
    MUTATE_EDIT,
    FETCH_WEB,
    TODO_WRITE,
];

/// Build a production runtime rooted at `root` with all five Components enabled
/// and an empty write-deny list.
fn direct_local_runtime(root: &std::path::Path) -> ReservedRuntime {
    let mut policy = PolicyConfig::defaults_for_project_root(root.to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let enabled = COMPONENT_TOOLS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: root.to_path_buf(),
        tls_roots_der: Vec::new(),
        workspace_history: lancet_api::WorkspaceHistoryBacking::Disabled,
        enabled_tools: enabled,
        policy,
        // `enabled_tools` is admission stage one. Admission is fail-closed, so the
        // fixture grants every Tool exercised by this acceptance matrix.
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
            denied_capabilities: Default::default(),
            max_boundary: lancet_protocol::cell::Trust::Ex,
        },
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
    let enabled = vec![MUTATE_CREATE.to_string()];
    let runtime = ReservedRuntime::build(LancetRuntimeConfig {
        local_workspace_root: dir.path().to_path_buf(),
        tls_roots_der: Vec::new(),
        workspace_history: lancet_api::WorkspaceHistoryBacking::Disabled,
        enabled_tools: enabled,
        policy,
        // Admit the exact Component under test so rejection reaches the guarded
        // workspace policy rather than stopping at catalog selection.
        capability_admission: lancet_protocol::evidence::CapabilityAdmissionPolicy {
            enabled_tools: std::iter::once(MUTATE_CREATE.to_string()).collect(),
            ..Default::default()
        },
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
    assert_eq!(err.code, CepErrorCode::CapabilityViolation, "{err:?}");
    assert_eq!(err.retriability, Retriability::Permanent, "{err:?}");
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

#[tokio::test]
async fn edit_failures_preserve_diagnostics_and_target_contents() {
    struct EditCase {
        name: &'static str,
        edits: serde_json::Value,
        code: Option<CepErrorCode>,
        retriability: Option<Retriability>,
        message_contains_any: &'static [&'static str],
    }
    for (name, path, body, fixed_hash, cases, compare_bytes) in [
        (
            "stale_hash",
            "edit.txt",
            "line one\nline two\n",
            // A deliberately wrong hash (the Local Revision has drifted) must replan.
            Some("blake3:0000000000000000000000000000000000000000000000000000000000000000"),
            vec![EditCase {
                name: "stale",
                edits: serde_json::json!([{"op": "replace_text", "old": "line one", "new": "line ONE"}]),
                code: None,
                retriability: Some(Retriability::RequiresReplan),
                message_contains_any: &["stale", "expected_hash"],
            }],
            true,
        ),
        (
            // Guest-owned diagnostics retain their structured CEP classes across the Component boundary.
            "semantic_diagnostics",
            "diag.txt",
            "same\nsame\n",
            None,
            vec![
                EditCase {
                    name: "ambiguous",
                    edits: serde_json::json!([{"op": "replace_text", "old": "same", "new": "other"}]),
                    code: Some(CepErrorCode::ReplanRequired),
                    retriability: None,
                    message_contains_any: &[],
                },
                EditCase {
                    name: "no_op",
                    edits: serde_json::json!([{"op": "replace_text", "old": "same", "new": "same", "occurrence": 1}]),
                    code: Some(CepErrorCode::SchemaMismatch),
                    retriability: None,
                    message_contains_any: &[],
                },
                EditCase {
                    name: "oversized_occurrence",
                    // Preserve an occurrence outside the wasm32 index range.
                    edits: serde_json::json!([{"op": "replace_text", "old": "same", "new": "other", "occurrence": 4294967297_u64}]),
                    code: Some(CepErrorCode::SchemaMismatch),
                    retriability: None,
                    message_contains_any: &[],
                },
            ],
            false,
        ),
    ] {
        let dir = TempDir::new().unwrap_or_else(|err| panic!("{name}: temp directory: {err}"));
        let target = dir.path().join(path);
        std::fs::write(&target, body).unwrap_or_else(|err| panic!("{name}: seed target: {err}"));
        let runtime = direct_local_runtime(dir.path());
        let client = runtime.client();
        let expected_hash = fixed_hash
            .map(str::to_owned)
            .unwrap_or_else(|| format!("blake3:{}", blake3::hash(body.as_bytes()).to_hex()));
        for case in cases {
            let case_name = case.name;
            let err = client
                .call_raw("mutate.edit", serde_json::json!({"path": path, "expected_hash": expected_hash, "edits": case.edits}))
                .await
                .expect_err(&format!("{name}/{case_name}: edit must fail"));
            if let Some(code) = case.code {
                assert_eq!(err.code, code, "{name}/{case_name}");
            }
            if let Some(retriability) = case.retriability {
                assert_eq!(err.retriability, retriability, "{name}/{case_name}");
            }
            if !case.message_contains_any.is_empty() {
                assert!(
                    case.message_contains_any
                        .iter()
                        .any(|text| err.message.contains(text)),
                    "{name}/{case_name}: expected replan reason, got: {}",
                    err.message
                );
            }
        }
        if compare_bytes {
            let on_disk =
                std::fs::read(&target).unwrap_or_else(|err| panic!("{name}: read target: {err}"));
            assert_eq!(on_disk, body.as_bytes(), "{name}");
        } else {
            let on_disk = std::fs::read_to_string(&target)
                .unwrap_or_else(|err| panic!("{name}: read target: {err}"));
            assert_eq!(on_disk, body, "{name}");
        }
    }
}

#[tokio::test]
async fn component_mutation_install_failure_leaves_no_partial_target() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();

    // The fault must really reach the install: a process that can still write
    // into a mode 0555 directory (root, or a filesystem ignoring modes) would
    // make the call succeed or fail elsewhere, so refuse to pass vacuously.
    let probe = std::fs::write(locked.join(".permission-probe"), b"");
    assert_eq!(
        probe.map_err(|err| err.kind()),
        Err(std::io::ErrorKind::PermissionDenied),
        "test needs a non-root process on a filesystem that honours directory modes"
    );

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

/// Component receipts carry method-local Cell evidence: the admitted method,
/// the resolved target path, and host-computed MutationEvidence.
#[tokio::test]
async fn component_receipts_carry_method_local_evidence() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "audit-body\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let create = client
        .call_receipt(
            MUTATE_CREATE,
            serde_json::json!({
                "path": "b.txt",
                "content": "audit-body\n",
                "create_parents": false
            }),
        )
        .await
        .expect("component create receipt");
    create.result.expect("component create result");
    assert!(dir.path().join("b.txt").is_file());
    assert_eq!(create.evidence.admission.tool, MUTATE_CREATE);
    let create_effect = create
        .evidence
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

    let observe = client
        .call_receipt(
            OBSERVE_WORKSPACE,
            serde_json::json!({ "mode": "read_file", "path": "a.txt" }),
        )
        .await
        .expect("component read receipt");
    observe.result.expect("component read result");
    assert_eq!(observe.evidence.admission.tool, OBSERVE_WORKSPACE);
    assert!(
        observe
            .evidence
            .effects
            .effects
            .iter()
            .any(|effect| matches!(
                &effect.target,
                ObservedTarget::Path { path } if path == "a.txt"
            ))
    );
}
