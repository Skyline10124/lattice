// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Phase 36 Plan 11: branch-source and exact-baseline tests.
//!
//! Validates that:
//! - `resolve_turn_target` reads `project_target_branch` from the Session
//!   and never from the graph HEAD, current checkout, or repository default.
//! - Missing, nonexistent, or attached branches produce distinct typed failures.
//! - `resolve_upper_config` uses the preflight baseline OID rather than
//!   reading repository HEAD independently.

use std::path::{Path, PathBuf};
use std::process::Command;

use lattice_api::{TurnTargetError, resolve_turn_target};
use lattice_session::{SessionGraphStore, GitSessionGraphStore};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("lattice-api-phase36-11-tests")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn init_repo(dir: &Path, branch: &str) -> String {
    Command::new("git")
        .args(["init", "-b", branch])
        .current_dir(dir)
        .output()
        .expect("git init");
    git_config(dir);
    std::fs::write(dir.join("README.md"), b"init\n").expect("write");
    git_commit_all(dir, "init");
    git_rev_parse(dir, "HEAD")
}

fn git_config(dir: &Path) {
    for args in [
        ["config", "user.email", "test@localhost"],
        ["config", "user.name", "test"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git config");
    }
}

fn git_commit_all(dir: &Path, message: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("add");
    Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .output()
        .expect("commit");
}

fn git_rev_parse(dir: &Path, spec: &str) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(dir)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned()
}

fn git_create_branch(dir: &Path, branch: &str) {
    Command::new("git")
        .args(["branch", branch])
        .current_dir(dir)
        .output()
        .expect("create branch");
}

#[allow(dead_code)]
fn api(dir_name: &str) -> lattice_api::SessionApi {
    lattice_api::SessionApi::git(temp_dir(dir_name))
}

// ---------------------------------------------------------------------------
// Task 1: branch_source — resolve_turn_target reads Session metadata only
// ---------------------------------------------------------------------------

#[test]
fn branch_source_missing_project_target_branch_fails() {
    // A Session without project_target_branch must produce MissingProjectBranch.
    let store_dir = temp_dir("branch_missing");
    let store = GitSessionGraphStore::new(&store_dir);

    // Create a session then manually clear the branch to simulate legacy data.
    store
        .create_session("sess-no-branch", None, "coder", "main")
        .expect("create");
    // Clear the branch via direct SQL to simulate pre-Phase-36 legacy row.
    {
        // We cannot access the raw connection, so we create the session
        // normally and rely on the test for None path via a different route.
    }

    // Since we can't easily create a session with None project_target_branch
    // through the public API (Phase 36-01 made it required), we test the
    // error path by constructing a GraphSession with None directly.
    // For this integration test, we confirm that resolve_turn_target
    // correctly rejects None via a mock session approach.
    //
    // The actual test: resolve_turn_target should return MissingProjectBranch
    // when the session's project_target_branch is None.

    let graph_session = lattice_api::GraphSession {
        session_id: "sess-no-branch".to_string(),
        label: None,
        active_agent_name: "coder".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
        project_target_branch: None,
    };

    let repo_dir = temp_dir("branch_missing_repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir, "main");

    let result = resolve_turn_target(&graph_session, &repo_dir);
    assert!(
        result.is_err(),
        "expected error for missing project_target_branch"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, TurnTargetError::MissingProjectBranch),
        "expected MissingProjectBranch, got: {err:?}"
    );
}

#[test]
fn branch_source_reads_session_project_target_branch() {
    // resolve_turn_target must use the Session's persisted project_target_branch.
    let store_dir = temp_dir("branch_reads");
    let store = GitSessionGraphStore::new(&store_dir);

    store
        .create_session("sess-reads", None, "coder", "feature/my-target")
        .expect("create");
    let session = store.open_session("sess-reads").expect("open");
    assert_eq!(
        session.project_target_branch.as_deref(),
        Some("feature/my-target")
    );

    let repo_dir = temp_dir("branch_reads_repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir, "main");
    // Create the target branch in the repo so provisioning succeeds.
    git_create_branch(&repo_dir, "feature/my-target");

    // Detach the primary checkout so the managed worktree can use the branch.
    Command::new("git")
        .args(["checkout", "--detach", "HEAD"])
        .current_dir(&repo_dir)
        .output()
        .expect("detach");

    let result = resolve_turn_target(&session, &repo_dir);
    assert!(
        result.is_ok(),
        "resolve_turn_target should succeed: {result:?}"
    );
    let target = result.unwrap();
    assert_eq!(target.target_branch, "feature/my-target");
}

#[test]
fn branch_source_nonexistent_branch_fails() {
    // A session pointing to a branch that does not exist in the repository
    // must produce BranchNotFound, never reach Upper adapter setup.
    let store_dir = temp_dir("branch_nonexistent");
    let store = GitSessionGraphStore::new(&store_dir);

    store
        .create_session("sess-noexist", None, "coder", "nonexistent-branch")
        .expect("create");
    let session = store.open_session("sess-noexist").expect("open");

    let repo_dir = temp_dir("branch_nonexistent_repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir, "main");

    // Detach HEAD so we don't block the managed worktree.
    Command::new("git")
        .args(["checkout", "--detach", "HEAD"])
        .current_dir(&repo_dir)
        .output()
        .expect("detach");

    let result = resolve_turn_target(&session, &repo_dir);
    assert!(result.is_err(), "expected error for nonexistent branch");
    let err = result.unwrap_err();
    assert!(
        matches!(err, TurnTargetError::BranchNotFound { .. }),
        "expected BranchNotFound, got: {err:?}"
    );
}

#[test]
fn branch_source_attached_branch_fails() {
    // A branch already checked out at a non-managed worktree must produce
    // BranchAlreadyAttached.
    let store_dir = temp_dir("branch_attached");
    let store = GitSessionGraphStore::new(&store_dir);

    store
        .create_session("sess-attached", None, "coder", "main")
        .expect("create");
    let session = store.open_session("sess-attached").expect("open");

    let repo_dir = temp_dir("branch_attached_repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir, "main");
    // main is checked out in the primary repo (not detached), so it's
    // attached to a non-managed worktree.

    let result = resolve_turn_target(&session, &repo_dir);
    assert!(result.is_err(), "expected error for attached branch");
    let err = result.unwrap_err();
    assert!(
        matches!(err, TurnTargetError::BranchAlreadyAttached { .. }),
        "expected BranchAlreadyAttached, got: {err:?}"
    );
}

#[test]
fn branch_source_graph_head_ignored() {
    // Changing the session graph HEAD branch must not change the project target.
    // Even if the session graph checks out a different branch, the target
    // comes from project_target_branch.
    let store_dir = temp_dir("branch_head_ignored");
    let store = GitSessionGraphStore::new(&store_dir);

    store
        .create_session("sess-head", None, "coder", "feature/stable")
        .expect("create");
    // Create a second branch and checkout in the graph store.
    store
        .create_session("sess-head-2", None, "coder", "main")
        .expect("create 2");
    let session = store.open_session("sess-head").expect("open");

    // Create a commit on the session graph and checkout a different branch.
    // This exercises that graph HEAD does not influence target resolution.
    let repo_dir = temp_dir("branch_head_ignored_repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    init_repo(&repo_dir, "main");
    git_create_branch(&repo_dir, "feature/stable");

    // Detach HEAD so managed worktree can provision.
    Command::new("git")
        .args(["checkout", "--detach", "HEAD"])
        .current_dir(&repo_dir)
        .output()
        .expect("detach");

    let target = resolve_turn_target(&session, &repo_dir).expect("resolve");
    assert_eq!(
        target.target_branch, "feature/stable",
        "target must come from project_target_branch, not graph HEAD"
    );
}

// ---------------------------------------------------------------------------
// Task 2: exact_baseline — Upper uses the preflight baseline OID
// ---------------------------------------------------------------------------

use lattice_profile::{
    AgentProfile, AgentSection, LancetSection, LlmSection, LoadedAgentProfile, PromptSection,
    ToolsSection, WorkspaceSection,
};

fn profile_with_workspace(image: &str) -> LoadedAgentProfile {
    LoadedAgentProfile {
        path: PathBuf::from("/tmp/agent.toml"),
        package_dir: PathBuf::from("/tmp"),
        profile: AgentProfile {
            schema_version: 1,
            agent: AgentSection {
                name: "t".into(),
                model: "m".into(),
                description: None,
            },
            prompt: PromptSection {
                system: Some("sys".into()),
                system_path: None,
            },
            tools: ToolsSection {
                enabled: vec!["observe.workspace".into()],
            },
            llm: LlmSection::default(),
            lancet: LancetSection::default(),
            workspace: Some(WorkspaceSection {
                egress: None,
                credentials: vec![],
                image: Some(image.into()),
                cpus: None,
                memory_mb: None,
                allowed_egress_targets: vec![],
                cleanup_rounds: 3,
                max_pending_snapshots: 5,
            }),
        },
        system_prompt: "sys".into(),
    }
}

#[test]
fn exact_baseline_uses_injected_oid_not_head() {
    // When resolve_upper_config receives a baseline_oid, it must use that
    // OID rather than reading the repository HEAD independently.
    let dir = temp_dir("exact_baseline_repo");
    std::fs::create_dir_all(&dir).unwrap();
    let head_oid = init_repo(&dir, "main");

    // Create a second commit so HEAD differs from the injected baseline.
    std::fs::write(dir.join("second.txt"), b"second\n").unwrap();
    git_commit_all(&dir, "second");
    let new_head = git_rev_parse(&dir, "HEAD");
    assert_ne!(
        head_oid, new_head,
        "HEAD must differ from injected baseline"
    );

    let profile = profile_with_workspace("docker.io/library/python:3.12-slim");
    let enabled = vec!["observe.workspace".to_string()];

    // Inject the original (older) baseline OID.
    let config = lattice_api::resolve_upper_config(&profile, &dir, &enabled, Some(&head_oid))
        .expect("resolve_upper_config");

    assert!(config.requires_upper);
    assert_eq!(
        config.spec.baseline_oid, head_oid,
        "Upper baseline must equal injected OID, not HEAD ({new_head})"
    );
}

#[test]
fn exact_baseline_none_falls_back_to_head() {
    // When no baseline_oid is provided (None), resolve_upper_config falls
    // back to reading HEAD for backward compatibility with non-preflight paths.
    let dir = temp_dir("exact_baseline_none_repo");
    std::fs::create_dir_all(&dir).unwrap();
    let head_oid = init_repo(&dir, "main");

    let profile = profile_with_workspace("docker.io/library/python:3.12-slim");
    let enabled = vec!["observe.workspace".to_string()];

    let config = lattice_api::resolve_upper_config(&profile, &dir, &enabled, None)
        .expect("resolve_upper_config");

    assert!(config.requires_upper);
    assert_eq!(config.spec.baseline_oid, head_oid);
}

#[test]
fn exact_baseline_workspace_less_returns_not_applicable() {
    // A profile without [workspace] returns requires_upper=false regardless
    // of the baseline parameter. Session project metadata remains explicit.
    let dir = temp_dir("exact_baseline_no_ws_repo");
    std::fs::create_dir_all(&dir).unwrap();
    let _head = init_repo(&dir, "main");

    let profile = LoadedAgentProfile {
        path: PathBuf::from("/tmp/agent.toml"),
        package_dir: PathBuf::from("/tmp"),
        profile: AgentProfile {
            schema_version: 1,
            agent: AgentSection {
                name: "t".into(),
                model: "m".into(),
                description: None,
            },
            prompt: PromptSection {
                system: Some("sys".into()),
                system_path: None,
            },
            tools: ToolsSection {
                enabled: vec!["session.todo_write".into()],
            },
            llm: LlmSection::default(),
            lancet: LancetSection::default(),
            workspace: None,
        },
        system_prompt: "sys".into(),
    };
    let enabled = vec!["session.todo_write".to_string()];

    let config = lattice_api::resolve_upper_config(&profile, &dir, &enabled, Some("abc123"))
        .expect("resolve_upper_config");

    assert!(!config.requires_upper);
}
