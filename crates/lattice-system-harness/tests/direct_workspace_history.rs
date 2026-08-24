// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local private workspace history (GitHub #63).
//!
//! Proves through the built `lattice` binary that a real turn which installs a
//! real Atomic Local Mutation lands that mutation in a private bare `gix`
//! repository outside the Local Workspace and outside the project `.git`, and
//! that a mutation which fails before installation records nothing.
//!
//! The history is read back with plain `gix` — a general Git library, not a
//! product crate — so this scenario stays inside the harness boundary. The
//! `TurnCommit.workspace_history_checkpoint` binding is deliberately *not* asserted
//! here: #63 forbids new user-facing command surface, so that field is not
//! observable through the CLI. It is covered at the library layer in
//! `crates/lattice-api/tests/workspace_history_acceptance.rs`.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use lattice_system_harness::{AssertError, ScenarioWorld, SseScript};
use support::{
    assert_clean_send, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, tool_failed, tool_ok, wire_fake_llm,
};

/// Locate the private history repository under the scenario's isolated HOME.
///
/// The world clears the environment and pins `HOME`, so the Linux XDG state
/// directory resolves beneath it. There is exactly one project key per
/// scenario, so a glob avoids re-deriving the product's hashing scheme here.
fn private_history_repo(world: &ScenarioWorld) -> Option<PathBuf> {
    let projects = world.home().join(".local/state/lattice/projects");
    let entries = fs::read_dir(&projects).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("workspace-history.git");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// The object id the private history's `current` ref points at.
fn current_tip(repo_path: &Path) -> Result<String, String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated())
        .map_err(|e| format!("open private history: {e}"))?;
    let reference = repo
        .try_find_reference("refs/lattice/workspace-history/current")
        .map_err(|e| format!("find current ref: {e}"))?
        .ok_or_else(|| "current ref is absent".to_string())?;
    Ok(reference
        .target()
        .try_id()
        .ok_or_else(|| "current ref has no object target".to_string())?
        .to_string())
}

/// Every file recorded at `commit_id`.
fn recorded_files_at(repo_path: &Path, commit_id: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated())
        .map_err(|e| format!("open private history: {e}"))?;
    let tip = gix::ObjectId::from_hex(commit_id.as_bytes())
        .map_err(|e| format!("parse object id: {e}"))?;
    let commit = repo
        .find_commit(tip)
        .map_err(|e| format!("find tip commit: {e}"))?;
    let tree = commit.tree().map_err(|e| format!("read tip tree: {e}"))?;
    let mut out = Vec::new();
    collect(&tree, "", &mut out)?;
    Ok(out)
}

/// Every file recorded at the private history's current tip.
fn recorded_files(repo_path: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let tip = current_tip(repo_path)?;
    recorded_files_at(repo_path, &tip)
}

fn collect(
    tree: &gix::Tree<'_>,
    prefix: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    for entry in tree.iter() {
        let entry = entry.map_err(|e| format!("iterate tree: {e}"))?;
        let name = entry.filename().to_string();
        let full = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let object = entry.object().map_err(|e| format!("load object: {e}"))?;
        match object.kind {
            gix::object::Kind::Tree => {
                let subtree = object
                    .try_into_tree()
                    .map_err(|e| format!("peel subtree: {e}"))?;
                collect(&subtree, &full, out)?;
            }
            gix::object::Kind::Blob => {
                let blob = object
                    .try_into_blob()
                    .map_err(|e| format!("peel blob: {e}"))?;
                out.push((full, blob.data.clone()));
            }
            _ => {}
        }
    }
    Ok(())
}

/// The per-record pin refs that keep prior object ids reachable.
fn pinned_records(repo_path: &Path) -> Result<Vec<String>, String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated())
        .map_err(|e| format!("open private history: {e}"))?;
    let platform = repo.references().map_err(|e| format!("refs: {e}"))?;
    let iter = platform
        .prefixed("refs/lattice/workspace-history/records/")
        .map_err(|e| format!("prefix refs: {e}"))?;
    Ok(iter
        .filter_map(|r| r.ok())
        .map(|r| r.name().as_bstr().to_string())
        .collect())
}

/// Sorted `(ref name, target)` pairs in a repository, for change detection.
fn ref_snapshot(repo_path: &Path) -> Vec<(String, String)> {
    let Ok(repo) = gix::open_opts(repo_path, gix::open::Options::isolated()) else {
        return Vec::new();
    };
    let Ok(platform) = repo.references() else {
        return Vec::new();
    };
    let Ok(all) = platform.all() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = all
        .filter_map(|r| r.ok())
        .map(|r| {
            (
                r.name().as_bstr().to_string(),
                r.target()
                    .try_id()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            )
        })
        .collect();
    out.sort();
    out
}

/// A real turn that installs a mutation records it in the private history,
/// leaves the project `.git` untouched, and keeps the history out of the
/// Local Workspace.
#[test]
fn direct_workspace_history_records_installed_mutation() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-wshist-create",
        "dl-wshist",
        "[\"mutate.create\"]",
        "",
    );
    world
        .write_project_file("seed.txt", "seed\n")
        .expect("seed file");
    world.commit_all("seed").expect("commit seed");

    let project_git = world.project_dir().join(".git");
    let refs_before = ref_snapshot(&project_git);

    let server = wire_fake_llm(
        &world,
        "dl-wshist",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"notes/history.md","content":"recorded\n","create_parents":true}"#,
            ),
            SseScript::plain("created"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "dl-wshist", "main")
            .expect("session");

    let turn = send.send("create the note").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            tool_ok(&turn.parsed, "mutate.create"),
            "create must succeed",
        )?;
        require(
            turn.has_committed_completed(),
            format!("turn incomplete: {:?}", turn.type_sequence()),
        )?;

        // The mutation really landed in the Local Workspace.
        let target = world.project_dir().join("notes/history.md");
        require(
            fs::read_to_string(&target).unwrap_or_default() == "recorded\n",
            format!("mutation bytes wrong: {:?}", fs::read(&target)),
        )?;

        // A private bare history repository exists outside the Local Workspace.
        let repo_path = match private_history_repo(&world) {
            Some(path) => path,
            None => {
                require(false, "no private workspace history repository was created")?;
                unreachable!("require(false) returns Err")
            }
        };
        require(
            !repo_path.starts_with(world.project_dir()),
            format!(
                "history must live outside the Local Workspace: {}",
                repo_path.display()
            ),
        )?;
        require(
            repo_path.join("HEAD").is_file() && !repo_path.join(".git").exists(),
            "history repository must be bare",
        )?;

        // It records exactly the installed mutation.
        let recorded = recorded_files(&repo_path).map_err(|e| AssertError::StdoutMissing {
            expected: format!("readable private history: {e}"),
        })?;
        require(
            recorded.len() == 1,
            format!(
                "expected exactly the mutated path, got {:?}",
                recorded.iter().map(|(p, _)| p).collect::<Vec<_>>()
            ),
        )?;
        require(
            recorded[0].0 == "notes/history.md" && recorded[0].1 == b"recorded\n",
            format!("recorded entry wrong: {:?}", recorded[0].0),
        )?;

        // The user's project Git is untouched, and the history is not inside it.
        require(
            ref_snapshot(&project_git) == refs_before,
            "history recording must not move the user's project refs",
        )?;
        require(
            !repo_path.starts_with(&project_git),
            "history must live outside the project .git",
        )?;

        // #143: normal recording writes no per-record refs — everything it
        // appends is an ancestor of `current`, and that is what keeps prior
        // object ids reachable. Prove it survives a real prune while
        // `current` stands, and that no stray per-record namespace was
        // resurrected.
        let pins = pinned_records(&repo_path).map_err(|e| AssertError::StdoutMissing {
            expected: format!("readable pin refs: {e}"),
        })?;
        require(
            pins.is_empty(),
            format!("normal recording must not write per-record refs: {pins:?}"),
        )?;
        let tip = current_tip(&repo_path).map_err(|e| AssertError::StdoutMissing {
            expected: format!("readable current ref: {e}"),
        })?;
        let git = |args: &[&str]| -> Result<std::process::Output, AssertError> {
            world
                .command("git")
                .args(["--git-dir", &repo_path.display().to_string()])
                .args(args)
                .output()
                .map_err(|e| AssertError::StdoutMissing {
                    expected: format!("git {args:?} runs: {e}"),
                })
        };
        let gc = git(&["gc", "--prune=now", "--aggressive"])?;
        require(
            gc.status.success(),
            format!("git gc failed: {}", String::from_utf8_lossy(&gc.stderr)),
        )?;
        let after_gc =
            recorded_files_at(&repo_path, &tip).map_err(|e| AssertError::StdoutMissing {
                expected: format!("recorded history survives a prune: {e}"),
            })?;
        require(
            after_gc == recorded,
            "recorded history must stay reachable through a prune",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("mutation recorded in private history", &turn.command)],
        captures,
        result,
    );
}

/// A mutation that fails its precondition installs nothing and records
/// nothing: the private history stays empty.
#[test]
fn direct_workspace_history_records_nothing_for_a_failed_mutation() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-wshist-fail",
        "dl-wshist-fail",
        "[\"mutate.edit\"]",
        "",
    );
    world
        .write_project_file("tracked.txt", "hello world\n")
        .expect("seed file");
    world.commit_all("seed tracked").expect("commit seed");

    let server = wire_fake_llm(
        &world,
        "dl-wshist-fail",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.edit",
                // A stale Local Revision: the Mutation Precondition fails
                // before anything is installed.
                r#"{"path":"tracked.txt","expected_hash":"blake3:0000000000000000000000000000000000000000000000000000000000000000","edits":[{"op":"replace_text","old":"hello","new":"goodbye"}]}"#,
            ),
            SseScript::plain("could not edit"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = lattice_system_harness::SessionSendDriver::new(
        &driver,
        &world,
        &spec,
        "dl-wshist-fail",
        "main",
    )
    .expect("session");

    let turn = send.send("edit with a stale hash").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            tool_failed(&turn.parsed, "mutate.edit"),
            "stale precondition must fail the edit",
        )?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "hello world\n",
            "a failed precondition must preserve the target bytes",
        )?;

        // Either no history repository was created at all, or it holds no
        // recorded mutation. Both mean the current ref never moved.
        match private_history_repo(&world) {
            None => Ok(()),
            Some(repo_path) => {
                let recorded = recorded_files(&repo_path);
                require(
                    recorded.is_err(),
                    format!(
                        "a pre-install failure must record nothing, found {:?}",
                        recorded.map(|r| r.into_iter().map(|(p, _)| p).collect::<Vec<_>>())
                    ),
                )
            }
        }
    })();

    finish_scenario(
        scenario_evidence,
        &[("failed mutation records nothing", &turn.command)],
        captures,
        result,
    );
}
