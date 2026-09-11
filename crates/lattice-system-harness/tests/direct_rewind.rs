// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Explicit Rewind through the built binary (GitHub #64).
//!
//! Proves at the product level that `lattice session rewind` reverses only the
//! Atomic Local Mutations LATTICE installed after the selected Turn, that the
//! conversation target and the Local Workspace end up agreeing, that external
//! changes LATTICE never touched survive, and that a diverged LATTICE-touched
//! path makes the whole Rewind report a conflict while nothing moves.
//!
//! The private history is read back with plain `gix` and content hashes with
//! plain `blake3` — general libraries, not product crates — so the scenario
//! stays inside the harness boundary.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use lattice_system_harness::{AssertError, ScenarioWorld, SendOutcome, SseScript};
use serde_json::Value;
use support::{
    assert_clean_send, cli, direct_world, driver, evidence, finish_scenario, require, sequence,
    tool_call, tool_ok, wire_fake_llm,
};

/// Locate the private history repository under the scenario's isolated HOME.
fn private_history_repo(world: &ScenarioWorld) -> Option<PathBuf> {
    let projects = world.home().join(".local/state/lattice/projects");
    for entry in fs::read_dir(&projects).ok()?.flatten() {
        let candidate = entry.path().join("workspace-history.git");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

/// The object id the private history's `current` ref points at.
fn current_tip(repo_path: &Path) -> Option<String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated()).ok()?;
    let reference = repo
        .try_find_reference("refs/lattice/workspace-history/current")
        .ok()??;
    Some(reference.target().try_id()?.to_string())
}

/// Every file recorded at `commit_id` in the private history.
fn recorded_files_at(repo_path: &Path, commit_id: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated())
        .map_err(|e| format!("open private history: {e}"))?;
    let oid = gix::ObjectId::from_hex(commit_id.as_bytes())
        .map_err(|e| format!("parse object id: {e}"))?;
    let commit = repo
        .find_commit(oid)
        .map_err(|e| format!("find commit: {e}"))?;
    let tree = commit.tree().map_err(|e| format!("read tree: {e}"))?;
    let mut out = Vec::new();
    collect(&tree, "", &mut out)?;
    Ok(out)
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

/// 使用公开固定结果的提交身份，不要求有损观察流必达末尾事件。
fn committed_id(outcome: &SendOutcome) -> Option<String> {
    outcome.committed_result().map(|(_, id, _)| id.to_owned())
}

fn local_revision(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn read_project(world: &ScenarioWorld, rel: &str) -> Option<Vec<u8>> {
    fs::read(world.project_dir().join(rel)).ok()
}

/// Commit ids listed by `session log --json`, oldest first.
fn session_log_ids(stdout: &str) -> Vec<String> {
    serde_json::from_str::<Value>(stdout)
        .ok()
        .and_then(|value| {
            value
                .get("turns")
                .or(Some(&value))
                .and_then(Value::as_array)
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| {
                            entry
                                .get("commit_id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect()
                })
        })
        .unwrap_or_default()
}

/// A Rewind through the CLI reverses the later LATTICE mutations, leaves every
/// external change alone, and lands the conversation target on the same Turn
/// the Local Workspace now reflects.
#[test]
fn direct_rewind_reverses_later_mutations_and_preserves_external_changes() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-rewind-apply",
        "dl-rewind",
        "[\"mutate.create\",\"mutate.edit\"]",
        "",
    );
    world
        .write_project_file(".gitignore", "secret.env\n")
        .expect("gitignore");
    world
        .write_project_file("secret.env", "TOKEN=seed\n")
        .expect("ignored file");
    world
        .write_project_file("seed.txt", "seed\n")
        .expect("seed file");
    world
        .write_project_file("doomed.txt", "delete me\n")
        .expect("doomed file");
    world.commit_all("seed").expect("commit seed");

    let project_git = world.project_dir().join(".git");
    let refs_before = ref_snapshot(&project_git);

    // Three Turns: create, create, edit. Each Turn is two LLM requests.
    let server = wire_fake_llm(
        &world,
        "dl-rewind",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"notes/a.md","content":"v1\n","create_parents":true}"#,
            ),
            SseScript::plain("created a"),
            tool_call(
                "c2",
                "mutate.create",
                r#"{"path":"notes/b.md","content":"b1\n","create_parents":true}"#,
            ),
            SseScript::plain("created b"),
            tool_call(
                "c3",
                "mutate.edit",
                &format!(
                    r#"{{"path":"notes/a.md","expected_hash":"{}","edits":[{{"op":"replace_text","old":"v1","new":"v2"}}]}}"#,
                    local_revision(b"v1\n")
                ),
            ),
            SseScript::plain("edited a"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "dl-rewind", "main")
            .expect("session");

    let first = send.send("create a").expect("turn 1");
    let second = send.send("create b").expect("turn 2");
    let third = send.send("edit a").expect("turn 3");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        for (label, turn) in [("1", &first), ("2", &second), ("3", &third)] {
            assert_clean_send(turn)?;
            require(
                turn.has_committed_completed(),
                format!("turn {label} incomplete: {:?}", turn.type_sequence()),
            )?;
        }
        require(tool_ok(&first.parsed, "mutate.create"), "turn 1 create ok")?;
        require(tool_ok(&second.parsed, "mutate.create"), "turn 2 create ok")?;
        require(tool_ok(&third.parsed, "mutate.edit"), "turn 3 edit ok")?;

        let target = committed_id(&first.parsed).ok_or_else(|| AssertError::StdoutMissing {
            expected: "turn 1 commit id".into(),
        })?;

        // The three mutations really landed.
        require(
            read_project(&world, "notes/a.md").as_deref() == Some(b"v2\n"),
            "turn 3 must have edited notes/a.md",
        )?;
        require(
            read_project(&world, "notes/b.md").as_deref() == Some(b"b1\n"),
            "turn 2 must have created notes/b.md",
        )?;

        let repo_path = private_history_repo(&world).ok_or_else(|| AssertError::StdoutMissing {
            expected: "a private workspace history repository".into(),
        })?;
        let tip_before = current_tip(&repo_path).ok_or_else(|| AssertError::StdoutMissing {
            expected: "a private history current ref".into(),
        })?;

        // External activity LATTICE never saw.
        fs::write(world.project_dir().join("external.txt"), "mine\n").expect("external create");
        fs::write(world.project_dir().join("seed.txt"), "user edited\n").expect("external edit");
        fs::remove_file(world.project_dir().join("doomed.txt")).expect("external delete");
        fs::write(world.project_dir().join("secret.env"), "TOKEN=rotated\n").expect("ignored edit");

        let rewind = cli(
            &world,
            &spec,
            &["--json", "session", "rewind", send.session_id(), &target],
        );
        let rewind_stdout = String::from_utf8_lossy(&rewind.stdout).to_string();
        let rewind_stderr = String::from_utf8_lossy(&rewind.stderr).to_string();
        require(
            rewind.is_clean_exit(0),
            format!("rewind failed ({:?}): {rewind_stderr}", rewind.exit_code()),
        )?;
        let report: Value =
            serde_json::from_str(&rewind_stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("rewind --json must emit one JSON object ({e}): {rewind_stdout}"),
            })?;
        require(
            report.get("target_commit_id").and_then(Value::as_str) == Some(target.as_str()),
            format!("rewind report names the target turn: {rewind_stdout}"),
        )?;
        let steps = report
            .get("steps")
            .and_then(Value::as_array)
            .map(|s| s.len())
            .unwrap_or(0);
        require(
            steps == 2,
            format!("exactly the two later mutations are reversed, got {steps}"),
        )?;

        // Local Workspace: LATTICE's later mutations are reversed and every
        // external change survives.
        require(
            read_project(&world, "notes/a.md").as_deref() == Some(b"v1\n"),
            format!(
                "the edit must be restored: {:?}",
                read_project(&world, "notes/a.md")
            ),
        )?;
        require(
            !world.project_dir().join("notes/b.md").exists(),
            "the reversed create must be deleted",
        )?;
        require(
            read_project(&world, "external.txt").as_deref() == Some(b"mine\n"),
            "an unrelated external create must survive",
        )?;
        require(
            read_project(&world, "seed.txt").as_deref() == Some(b"user edited\n"),
            "an unrelated external edit must survive",
        )?;
        require(
            !world.project_dir().join("doomed.txt").exists(),
            "an unrelated external delete must stay deleted",
        )?;
        // Prove the survivor really is ignored content, not just another
        // unrelated file: ask the project's own Git.
        let ignored = world
            .command("git")
            .args(["check-ignore", "-q", "secret.env"])
            .status()
            .map_err(|e| AssertError::StdoutMissing {
                expected: format!("git check-ignore runs: {e}"),
            })?;
        require(
            ignored.success(),
            "secret.env must actually match an ignore rule",
        )?;
        // …and that it is genuinely untracked, so this is ignored content
        // rather than a committed file that merely matches a pattern.
        let tracked = world
            .command("git")
            .args(["ls-files", "--", "secret.env"])
            .output()
            .map_err(|e| AssertError::StdoutMissing {
                expected: format!("git ls-files runs: {e}"),
            })?;
        require(
            tracked.stdout.is_empty(),
            format!(
                "secret.env must be untracked ignored content, got: {}",
                String::from_utf8_lossy(&tracked.stdout)
            ),
        )?;
        require(
            read_project(&world, "secret.env").as_deref() == Some(b"TOKEN=rotated\n"),
            "ignored content LATTICE never touched must be untouched",
        )?;

        // Conversation target: the session log is back to the target Turn, so
        // it agrees with the Local Workspace.
        let log = cli(
            &world,
            &spec,
            &["--json", "session", "log", send.session_id()],
        );
        let log_stdout = String::from_utf8_lossy(&log.stdout).to_string();
        let ids = session_log_ids(&log_stdout);
        require(
            ids == vec![target.clone()],
            format!("session log must end at the target turn, got {ids:?}"),
        )?;

        // Private history: `current` names the target's recorded state.
        let tip_after = current_tip(&repo_path).ok_or_else(|| AssertError::StdoutMissing {
            expected: "a private history current ref after rewind".into(),
        })?;
        require(
            tip_after != tip_before,
            "the private history ref must have moved",
        )?;
        let recorded =
            recorded_files_at(&repo_path, &tip_after).map_err(|e| AssertError::StdoutMissing {
                expected: format!("readable private history: {e}"),
            })?;
        require(
            recorded == vec![("notes/a.md".to_string(), b"v1\n".to_vec())],
            format!("rewound history tip records only the target state: {recorded:?}"),
        )?;

        // The user's project Git is untouched by the whole flow.
        require(
            ref_snapshot(&project_git) == refs_before,
            "rewind must not move the user's project refs",
        )?;

        // Reachability: the reversed commits survive a prune of everything
        // unreachable, kept alive by their internal pins alone.
        let git = |args: &[&str]| {
            world
                .command("git")
                .args(["--git-dir", &repo_path.display().to_string()])
                .args(args)
                .output()
        };
        let gc = git(&["gc", "--prune=now", "--aggressive"]).map_err(|e| {
            AssertError::StdoutMissing {
                expected: format!("git gc runs: {e}"),
            }
        })?;
        require(
            gc.status.success(),
            format!("git gc failed: {}", String::from_utf8_lossy(&gc.stderr)),
        )?;
        let pre_rewind =
            recorded_files_at(&repo_path, &tip_before).map_err(|e| AssertError::StdoutMissing {
                expected: format!("the pre-rewind tip must survive a prune: {e}"),
            })?;
        require(
            pre_rewind.iter().any(|(path, _)| path == "notes/b.md"),
            format!("the rewound-away state stays reachable: {pre_rewind:?}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("turn 1", &first.command),
            ("turn 2", &second.command),
            ("turn 3", &third.command),
        ],
        captures,
        result,
    );
}

/// A LATTICE-touched path the user has since changed makes the whole Rewind
/// report a conflict: the Local Workspace, the private history ref, and the
/// conversation target all stay exactly where they were.
#[test]
fn direct_rewind_conflict_moves_nothing() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-rewind-conflict",
        "dl-rewind-conflict",
        "[\"mutate.create\"]",
        "",
    );
    world
        .write_project_file("seed.txt", "seed\n")
        .expect("seed file");
    world.commit_all("seed").expect("commit seed");

    let server = wire_fake_llm(
        &world,
        "dl-rewind-conflict",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"notes/a.md","content":"v1\n","create_parents":true}"#,
            ),
            SseScript::plain("created a"),
            tool_call(
                "c2",
                "mutate.create",
                r#"{"path":"notes/b.md","content":"b1\n","create_parents":true}"#,
            ),
            SseScript::plain("created b"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = lattice_system_harness::SessionSendDriver::new(
        &driver,
        &world,
        &spec,
        "dl-rewind-conflict",
        "main",
    )
    .expect("session");

    let first = send.send("create a").expect("turn 1");
    let second = send.send("create b").expect("turn 2");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(tool_ok(&first.parsed, "mutate.create"), "turn 1 create ok")?;
        require(tool_ok(&second.parsed, "mutate.create"), "turn 2 create ok")?;
        let target = committed_id(&first.parsed).ok_or_else(|| AssertError::StdoutMissing {
            expected: "turn 1 commit id".into(),
        })?;
        let head = committed_id(&second.parsed).ok_or_else(|| AssertError::StdoutMissing {
            expected: "turn 2 commit id".into(),
        })?;

        // The user rewrites a path LATTICE created.
        fs::write(world.project_dir().join("notes/b.md"), "hand written\n")
            .expect("external edit of a LATTICE path");

        let repo_path = private_history_repo(&world).ok_or_else(|| AssertError::StdoutMissing {
            expected: "a private workspace history repository".into(),
        })?;
        let tip_before = current_tip(&repo_path).ok_or_else(|| AssertError::StdoutMissing {
            expected: "a private history current ref".into(),
        })?;

        let rewind = cli(
            &world,
            &spec,
            &["--json", "session", "rewind", send.session_id(), &target],
        );
        let stdout = String::from_utf8_lossy(&rewind.stdout).to_string();
        require(
            rewind.is_clean_exit(1),
            format!(
                "a conflict must exit 1, got {:?}: {stdout}",
                rewind.exit_code()
            ),
        )?;
        let report: Value =
            serde_json::from_str(&stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("conflict --json must emit one JSON object ({e}): {stdout}"),
            })?;
        let conflicts = report
            .get("conflicts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        require(
            conflicts.len() == 1
                && conflicts[0].get("path").and_then(Value::as_str) == Some("notes/b.md"),
            format!("the conflict names the diverged path: {stdout}"),
        )?;
        require(
            !stdout.contains("hand written"),
            format!("the conflict report must not carry file content: {stdout}"),
        )?;

        // Nothing moved.
        require(
            read_project(&world, "notes/b.md").as_deref() == Some(b"hand written\n"),
            "the user's content must be preserved",
        )?;
        require(
            read_project(&world, "notes/a.md").as_deref() == Some(b"v1\n"),
            "matching paths must not be written either",
        )?;
        require(
            current_tip(&repo_path).as_deref() == Some(tip_before.as_str()),
            "the private history ref must not move on a conflict",
        )?;
        let log = cli(
            &world,
            &spec,
            &["--json", "session", "log", send.session_id()],
        );
        let ids = session_log_ids(&String::from_utf8_lossy(&log.stdout));
        require(
            ids.last() == Some(&head),
            format!("the conversation target must not move: {ids:?}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("turn 1", &first.command), ("turn 2", &second.command)],
        captures,
        result,
    );
}
