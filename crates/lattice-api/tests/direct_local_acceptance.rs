// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! In-process acceptance for the exact error attributes and receipt evidence of
//! Direct-Local mutations: policy denial and existing-target refusals map to
//! the right retriability, stale hashes and edit diagnostics keep their
//! structured codes, a failed install leaves no partial target, and receipts
//! carry method-local MutationEvidence. Product scenarios live in the external
//! harness; see `docs/development/testing.md`.

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
            serde_json::json!({"request": { "mode": "read_file", "path": "a.txt" }}),
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
