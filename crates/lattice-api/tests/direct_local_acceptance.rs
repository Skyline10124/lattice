// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! #37 acceptance tests: production Turn operates directly on the Local
//! Workspace through the closed five-method catalog.
//!
//! These tests exercise the real production runtime
//! (`lancet_api::LancetRuntime::build` with a temp Local root, no Upper, no
//! workspace override, no KVM/MSB) and assert each acceptance criterion from
//! the #37 ticket against the canonical project root on disk:
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
use lancet_catalog::methods::workspace::{
    Create, CreateParams, Edit, EditOperation, EditParams, ObserveWorkspace,
    ObserveWorkspaceParams, ReadFileParams,
};
use lancet_cep::ToolExposure;
use lancet_protocol::Retriability;
use tempfile::TempDir;

/// Build a production runtime rooted at `root` with all five catalog methods
/// enabled and an empty write-deny list.
fn direct_local_runtime(root: &std::path::Path) -> LancetRuntime {
    let mut policy = PolicyConfig::defaults_for_project_root(root.to_path_buf());
    policy.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    LancetRuntime::build(LancetRuntimeConfig {
        project_root: root.to_path_buf(),
        enabled_tools: enabled,
        policy,
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
    // ToolExposure must also agree (rejects unknown, admits exactly the five).
    let exposure = ToolExposure::from_catalog_enabled(&names).expect("exposure");
    assert_eq!(exposure.definitions().len(), 5);
}

/// observe.workspace read_file sees the current Local state, including an
/// external modification made between two reads.
#[tokio::test]
async fn observe_sees_external_modification_between_tool_calls() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("watch.txt"), "v1\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let first = client
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "watch.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }))
        .await
        .expect("first read");
    let lancet_catalog::methods::workspace::ObserveWorkspaceResult::ReadFile(first) = first else {
        panic!("read_file result");
    };
    assert!(first.content.contains("v1"));

    // External writer (the user's editor) changes the file on disk between
    // Tool calls. Direct-Local must observe the new state on the next read.
    std::fs::write(dir.path().join("watch.txt"), "v2-external\n").unwrap();

    let second = client
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "watch.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }))
        .await
        .expect("second read");
    let lancet_catalog::methods::workspace::ObserveWorkspaceResult::ReadFile(second) = second
    else {
        panic!("read_file result");
    };
    assert!(
        second.content.contains("v2-external"),
        "second read must see the external modification: {}",
        second.content
    );
}

/// observe.workspace list_directory sees a file added by an external writer
/// between two listings. Guards list/grep/find against stale snapshots.
#[tokio::test]
async fn observe_list_sees_external_addition_between_tool_calls() {
    use lancet_catalog::methods::workspace::{ListDirectoryParams, ObserveWorkspaceResult};

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("orig.txt"), "x\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let first = client
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ListDirectory(ListDirectoryParams {
            path: ".".into(),
            limit: None,
        }))
        .await
        .expect("first list");
    let ObserveWorkspaceResult::ListDirectory(first) = first else {
        panic!("list_directory result");
    };
    assert!(first.entries.iter().any(|e| e.name == "orig.txt"));
    assert!(!first.entries.iter().any(|e| e.name == "added.txt"));

    std::fs::write(dir.path().join("added.txt"), "new\n").unwrap();

    let second = client
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ListDirectory(ListDirectoryParams {
            path: ".".into(),
            limit: None,
        }))
        .await
        .expect("second list");
    let ObserveWorkspaceResult::ListDirectory(second) = second else {
        panic!("list_directory result");
    };
    assert!(
        second.entries.iter().any(|e| e.name == "added.txt"),
        "second list must see the externally added file: {:?}",
        second.entries
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
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let runtime = LancetRuntime::build(LancetRuntimeConfig {
        project_root: dir.path().to_path_buf(),
        enabled_tools: enabled,
        policy,
        cep_mode: lancet_protocol::policy::CepMode::Enforce,
        audit: lancet_api::AuditBacking::InMemory,
        model_id: "test".into(),
        runtime_version: "test".into(),
    })
    .expect("runtime");
    let client = runtime.client();

    let err = client
        .call::<Create>(CreateParams {
            path: "secret/leak.txt".into(),
            content: "x".into(),
            create_parents: true,
        })
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
        .call::<Create>(CreateParams {
            path: "exists.txt".into(),
            content: "second".into(),
            create_parents: false,
        })
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
        .call::<Create>(CreateParams {
            path: "race.txt".into(),
            content: "agent\n".into(),
            create_parents: false,
        })
        .await
        .expect("first create on absent target");
    assert_eq!(
        std::fs::read(dir.path().join("race.txt")).unwrap(),
        b"agent\n"
    );

    // A second create on the now-existing target must fail with
    // ReplanRequired (no-replace install), leaving the first content intact.
    let err = client
        .call::<Create>(CreateParams {
            path: "race.txt".into(),
            content: "attacker\n".into(),
            create_parents: false,
        })
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
        .call::<Edit>(EditParams {
            path: "edit.txt".into(),
            expected_hash: stale_hash.into(),
            edits: vec![EditOperation::ReplaceText {
                old: "line one".into(),
                new: "line ONE".into(),
                occurrence: None,
            }],
        })
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
        .call::<Edit>(EditParams {
            path: "ok.txt".into(),
            expected_hash: expected,
            edits: vec![EditOperation::ReplaceText {
                old: "hello".into(),
                new: "goodbye".into(),
                occurrence: None,
            }],
        })
        .await
        .expect("edit installs");
    assert_eq!(result.changed_files.len(), 1);

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
        .call::<Create>(CreateParams {
            path: "survives.txt".into(),
            content: "persisted\n".into(),
            create_parents: false,
        })
        .await
        .expect("create");
    assert!(dir.path().join("survives.txt").exists());

    // 2. A later step fails (stale-hash edit on a different file).
    std::fs::write(dir.path().join("other.txt"), "x\n").unwrap();
    let stale = "blake3:0000000000000000000000000000000000000000000000000000000000000000";
    let _ = client
        .call::<Edit>(EditParams {
            path: "other.txt".into(),
            expected_hash: stale.into(),
            edits: vec![EditOperation::ReplaceText {
                old: "x".into(),
                new: "y".into(),
                occurrence: None,
            }],
        })
        .await;

    // The earlier successful create is still on disk — Direct-Local does not
    // pretend to roll back mutations when a later Tool call fails.
    let on_disk = std::fs::read(dir.path().join("survives.txt")).unwrap();
    assert_eq!(on_disk, b"persisted\n");
}

/// Audit records method-local evidence (guarded path + Local Revision) and
/// carries no routing identity: no `execution_backend`, no
/// `upper_workspace_id`, no adapter-preflight or generic routing invocation
/// flags (#38).
#[tokio::test]
async fn audit_records_method_local_evidence_without_routing_metadata() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.txt"), "audit-body\n").unwrap();
    let runtime = direct_local_runtime(dir.path());
    let client = runtime.client();

    let _ = client
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "a.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }))
        .await
        .expect("read");
    runtime.audit().await_pending().await;

    let rows = runtime
        .audit()
        .query_rows(&lancet_native::AuditQueryFilters::default())
        .expect("rows");
    assert!(!rows.is_empty(), "audit must record the call");
    let meta = rows[0].metadata_json.as_deref().unwrap_or("{}");
    // Method-local evidence: the request target, the guarded normalized
    // path, and the content hash (Local Revision) of the observed file.
    assert!(
        meta.contains("a.txt"),
        "audit metadata must record the real Local request path: {meta}"
    );
    assert!(
        meta.contains("\"guarded_path\":\"a.txt\""),
        "audit metadata must record the guarded normalized path: {meta}"
    );
    assert!(
        meta.contains("content_hash") && meta.contains("blake3:"),
        "audit metadata must record the Local Revision (content_hash): {meta}"
    );
    // No routing identity or invocation flags survive.
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
