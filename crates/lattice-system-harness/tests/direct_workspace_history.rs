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
//! 正常历史仍保存变更前后内容和 TurnId/InvocationId；会话退回不移动历史。

mod support;

use serde_json::Value;
use std::collections::BTreeMap;

use std::fs;
use std::path::{Path, PathBuf};

use lattice_system_harness::{AssertError, ScenarioWorld, SseScript};
use support::{
    assert_clean_send, direct_world, driver, evidence, finish_scenario, finished_with_code,
    require, sequence, tool_call, tool_failed, tool_ok, wire_fake_llm,
};

/// Locate the private history repository under the scenario's isolated HOME.
///
/// The world clears the environment and pins `HOME`, so the Linux XDG state
/// directory resolves beneath it. There is exactly one project key per
/// scenario, so a glob avoids re-deriving the product's hashing scheme here.
fn private_history_repo(world: &ScenarioWorld) -> PathBuf {
    let projects = world.home().join(".local/state/lattice/projects");
    let repos: Vec<_> = fs::read_dir(projects)
        .expect("read isolated projects")
        .map(|entry| {
            entry
                .expect("project entry")
                .path()
                .join("workspace-history.git")
        })
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(repos.len(), 1, "exactly one readable project history");
    repos.into_iter().next().unwrap()
}

fn history_tip(repo_path: &Path) -> Result<Option<String>, String> {
    let repo = gix::open_opts(repo_path, gix::open::Options::isolated())
        .map_err(|e| format!("open private history: {e}"))?;
    repo.try_find_reference("refs/lattice/workspace-history/current")
        .map_err(|e| format!("find current ref: {e}"))?
        .map(|reference| {
            reference
                .target()
                .try_id()
                .map(|id| id.to_string())
                .ok_or_else(|| "current ref has no object target".to_string())
        })
        .transpose()
}

/// The object id the private history's `current` ref points at.
fn current_tip(repo_path: &Path) -> Result<String, String> {
    history_tip(repo_path)?.ok_or_else(|| "current ref is absent".to_string())
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
fn tree_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(dir).expect("read snapshot directory") {
            let path = entry.expect("snapshot entry").path();
            if fs::symlink_metadata(&path)
                .expect("snapshot metadata")
                .is_dir()
            {
                walk(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(path).expect("snapshot bytes"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn mutation_chain(repo_path: &Path) -> Vec<(String, String)> {
    let repo =
        gix::open_opts(repo_path, gix::open::Options::isolated()).expect("open history chain");
    let tip = current_tip(repo_path).expect("history current");
    let mut next = Some(gix::ObjectId::from_hex(tip.as_bytes()).unwrap());
    let mut chain = Vec::new();
    while let Some(oid) = next {
        let commit = repo.find_commit(oid).expect("history commit");
        let message = commit.message_raw_sloppy().to_string();
        next = commit.parent_ids().next().map(|id| id.detach());
        if !message.contains("lattice-history-kind: baseline") {
            chain.push((oid.to_string(), message));
        }
    }
    chain.reverse();
    chain
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
        "[\"mutate.create\",\"mutate.edit\"]",
        "",
    );
    world
        .write_project_file("seed.txt", "seed\n")
        .expect("seed file");
    world.commit_all("seed").expect("commit seed");

    let project_git = world.project_dir().join(".git");
    let refs_before = ref_snapshot(&project_git);

    let hash = format!("blake3:{}", blake3::hash(b"recorded\n").to_hex());
    let edit_args = format!(
        r#"{{"path":"notes/history.md","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"recorded","new":"edited"}}]}}"#
    );
    let server = wire_fake_llm(
        &world,
        "dl-wshist",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"notes/history.md","content":"recorded\n","create_parents":true}"#,
            ),
            tool_call("e1", "mutate.edit", &edit_args),
            SseScript::plain("created"),
        ]),
    );
    let git_before = tree_snapshot(&project_git);
    let workspace_before = tree_snapshot(world.project_dir());
    assert!(
        !git_before.is_empty(),
        "fixture project Git must not be empty"
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "dl-wshist");

    let turn = send.send("create the note").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&turn)?;
        require(
            tool_ok(&turn.parsed, "mutate.create"),
            "create must succeed",
        )?;
        require(
            turn.has_completed(),
            format!("turn incomplete: {:?}", turn.type_sequence()),
        )?;

        // The mutation really landed in the Local Workspace.
        let target = world.project_dir().join("notes/history.md");
        require(
            fs::read_to_string(&target).unwrap_or_default() == "edited\n",
            format!("mutation bytes wrong: {:?}", fs::read(&target)),
        )?;

        // A private bare history repository exists outside the Local Workspace.
        let repo_path = private_history_repo(&world);
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
            recorded[0].0 == "notes/history.md" && recorded[0].1 == b"edited\n",
            format!("recorded entry wrong: {:?}", recorded[0].0),
        )?;
        require(tool_ok(&turn.parsed, "mutate.edit"), "edit must succeed")?;
        require(
            tree_snapshot(&project_git) == git_before,
            "history must preserve every project .git path and byte",
        )?;
        let workspace_after = tree_snapshot(world.project_dir());
        let added: Vec<_> = workspace_after
            .keys()
            .filter(|path| !workspace_before.contains_key(*path))
            .collect();
        require(
            added == vec![&PathBuf::from("notes/history.md")],
            format!("only the mutated path may appear in workspace: {added:?}"),
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

/// Refusals preserve a readable empty history; later success and refusals keep
/// the installed record without adding compensation commits.
#[test]
fn direct_workspace_history_records_nothing_for_a_failed_mutation() {
    let driver = driver();
    let stale_args = r#"{"path":"tracked.txt","expected_hash":"blake3:0000000000000000000000000000000000000000000000000000000000000000","edits":[{"op":"replace_text","old":"hello","new":"goodbye"}]}"#;
    for (case, method, args, profile, code) in [
        ("stale", "mutate.edit", stale_args, "", "ReplanRequired"),
        (
            "exists",
            "mutate.create",
            r#"{"path":"tracked.txt","content":"other\n","create_parents":false}"#,
            "",
            "ReplanRequired",
        ),
        (
            "guard",
            "mutate.create",
            r#"{"path":"blocked.txt","content":"nope\n","create_parents":false}"#,
            "[lancet.workspace]\ndenied_write_patterns = [\"blocked.txt\"]\n",
            "CapabilityViolation",
        ),
        (
            "capability",
            "mutate.create",
            r#"{"path":"denied.txt","content":"nope\n","create_parents":false}"#,
            "[lancet.capabilities]\ndeny = [\"workspace.mutation\"]\n",
            "CapabilityViolation",
        ),
    ] {
        let (world, spec) = direct_world(
            &format!("direct-wshist-fail-{case}"),
            "history-refusal",
            "[\"mutate.create\",\"mutate.edit\"]",
            profile,
        );
        world
            .write_project_file("tracked.txt", "hello world\n")
            .unwrap();
        world.commit_all("seed tracked").unwrap();
        let server = wire_fake_llm(
            &world,
            "history-refusal",
            sequence(vec![
                SseScript::plain("initial empty history"),
                tool_call("refused", method, args),
                SseScript::plain("could not mutate"),
                tool_call(
                    "success",
                    "mutate.create",
                    r#"{"path":"fresh.txt","content":"ok\n","create_parents":false}"#,
                ),
                SseScript::plain("created"),
                tool_call(
                    "exists-again",
                    "mutate.create",
                    r#"{"path":"fresh.txt","content":"again\n","create_parents":false}"#,
                ),
                SseScript::plain("refused again"),
                tool_call(
                    "stale-again",
                    "mutate.edit",
                    r#"{"path":"fresh.txt","expected_hash":"blake3:0000000000000000000000000000000000000000000000000000000000000000","edits":[{"op":"replace_text","old":"ok","new":"x"}]}"#,
                ),
                SseScript::plain("refused stale again"),
            ]),
        );
        let mut scenario_evidence = evidence(&world, &spec);
        let send = lattice_system_harness::SessionSendDriver::new(
            &driver,
            &world,
            &spec,
            "history-refusal",
        );
        let initial = send.send("initialize history").unwrap();
        scenario_evidence.record_command("initial history", &initial.command);
        let result: Result<(), AssertError> = (|| {
            assert_clean_send(&initial)?;
            let repo = private_history_repo(&world);
            require(
                history_tip(&repo).expect("read initial history").is_none(),
                "initial current must be absent",
            )?;
            let turn = send.send("try refused mutation").unwrap();
            scenario_evidence.record_command("refused mutation", &turn.command);
            assert_clean_send(&turn)?;
            require(
                tool_failed(&turn.parsed, method),
                format!("{case} must fail the tool"),
            )?;
            require(
                finished_with_code(&turn.parsed, method, code),
                format!("{case} must preserve {code}"),
            )?;
            let detail = send
                .run_command(&[
                    "--json",
                    "session",
                    "turn",
                    turn.accepted_turn_id().unwrap(),
                ])
                .unwrap();
            scenario_evidence.record_command("refusal persistent detail", &detail);
            require(detail.is_clean_exit(0), "refusal Turn detail readable")?;
            let detail: Value = serde_json::from_slice(&detail.stdout).unwrap();
            let step = &detail["steps"][0]["state"];
            require(
                step["state"] == "closed",
                "refusal first Step must be Closed",
            )?;
            let slots = step["slots"].as_array().expect("closed slots");
            require(
                slots.len() == 1,
                "refusal first Step must have exactly one slot",
            )?;
            require(
                slots[0]["binding"]["method"] == method,
                "refusal slot method must match",
            )?;
            require(
                slots[0]["state"]["state"] == "terminal"
                    && slots[0]["state"]["outcome"] == "failed",
                "refusal slot must persist Failed (not NotExecuted/Indeterminate)",
            )?;
            require(
                fs::read(world.project_dir().join("tracked.txt")).unwrap() == b"hello world\n",
                "refusal must preserve target bytes",
            )?;
            require(
                !world.project_dir().join("denied.txt").exists(),
                "capability denial must not install target",
            )?;
            require(
                history_tip(&repo).expect("read refused history").is_none(),
                format!("{case} refusal current must be absent"),
            )?;
            if case == "stale" {
                let success = send.send("now create fresh").unwrap();
                scenario_evidence.record_command("subsequent success", &success.command);
                assert_clean_send(&success)?;
                require(
                    tool_ok(&success.parsed, "mutate.create"),
                    "subsequent create succeeds",
                )?;
                let tip = current_tip(&repo).expect("success must record current");
                for method in ["mutate.create", "mutate.edit"] {
                    let retry = send.send("refuse after success").unwrap();
                    scenario_evidence.record_command(method, &retry.command);
                    assert_clean_send(&retry)?;
                    require(
                        tool_failed(&retry.parsed, method),
                        "later mutation must fail",
                    )?;
                    require(
                        current_tip(&repo).expect("read after later refusal") == tip,
                        "later refusal must not move current",
                    )?;
                }
                require(
                    recorded_files(&repo).unwrap() == vec![("fresh.txt".into(), b"ok\n".to_vec())],
                    "later refusal preserves installed history bytes",
                )?;
                require(
                    mutation_chain(&repo).len() == 1,
                    "no compensation commit may be appended",
                )?;
            }
            Ok(())
        })();
        finish_scenario(scenario_evidence, &[], server.captured_requests(), result);
    }
}

/// Multiple model steps retain installed effects, ordered trees and real
/// invocation identities even when the next provider request fails.
#[test]
fn direct_workspace_history_multi_mutation_survives_provider_failure() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-wshist-multi-failure",
        "history-multi",
        "[\"mutate.create\",\"mutate.edit\"]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let hash = format!("blake3:{}", blake3::hash(b"1\n").to_hex());
    let edit = format!(
        r#"{{"path":"one.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"1","new":"3"}}]}}"#
    );
    let server = wire_fake_llm(
        &world,
        "history-multi",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"one.txt","content":"1\n","create_parents":false}"#,
            ),
            tool_call(
                "c2",
                "mutate.create",
                r#"{"path":"two.txt","content":"2\n","create_parents":false}"#,
            ),
            tool_call("c3", "mutate.edit", &edit),
            SseScript::HttpError {
                status: 400,
                body: r#"{"error":{"message":"failed after installed mutations"}}"#.into(),
            },
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        lattice_system_harness::SessionSendDriver::new(&driver, &world, &spec, "history-multi");
    let outcome = send.send("do three things then fail").unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            outcome.command.is_clean_exit(1),
            "provider failure must exit 1",
        )?;
        require(detail.is_clean_exit(0), "failed Turn must remain readable")?;
        let detail: Value = serde_json::from_slice(&detail.stdout).unwrap();
        require(
            detail["summary"]["outcome"] == "failed",
            "multi-mutation Turn must be Failed",
        )?;
        let snapshot = outcome
            .events
            .iter()
            .rev()
            .find_map(|event| match event {
                lattice_system_harness::TurnEventLine::Unknown { type_name, raw }
                    if type_name == "snapshot" =>
                {
                    Some(&raw["view"]["selected_turn"])
                }
                _ => None,
            })
            .expect("final durable snapshot");
        require(
            detail["summary"] == snapshot["summary"],
            "reloaded summary must equal returned summary",
        )?;
        require(
            fs::read(world.project_dir().join("one.txt")).unwrap() == b"3\n"
                && fs::read(world.project_dir().join("two.txt")).unwrap() == b"2\n",
            "both installed files survive provider failure",
        )?;
        let steps = detail["steps"].as_array().unwrap();
        require(
            steps.len() == 4
                && steps[3]["state"]["state"] == "closed"
                && steps[3]["state"]["slots"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
                && steps[3]["state"]["end"]["cause"]["cause"] == "provider_error",
            "three Tool steps then provider error",
        )?;
        let repo = private_history_repo(&world);
        let chain = mutation_chain(&repo);
        require(
            chain.len() == 3,
            "exactly three mutation commits in history",
        )?;
        for (index, ((oid, message), method)) in chain
            .iter()
            .zip(["mutate.create", "mutate.create", "mutate.edit"])
            .enumerate()
        {
            let state = &steps[index]["state"];
            require(
                state["state"] == "closed" && state["end"]["end"] == "continue",
                "mutation Step must persist Closed/Continue",
            )?;
            let slots = state["slots"].as_array().unwrap();
            require(
                slots.len() == 1
                    && slots[0]["binding"]["method"] == method
                    && slots[0]["state"]["outcome"] == "succeeded",
                "each model Step must persist its single successful mutation",
            )?;
            let turn_id = detail["turn_id"].as_str().unwrap();
            let invocation_id = slots[0]["invocation_id"].as_str().unwrap();
            require(
                message.contains(turn_id),
                format!("history commit {index} must carry actual TurnId"),
            )?;
            require(
                message.contains(invocation_id),
                format!("history commit {index} must carry ordered actual InvocationId"),
            )?;
            let expected = match index {
                0 => vec![("one.txt".into(), b"1\n".to_vec())],
                1 => vec![
                    ("one.txt".into(), b"1\n".to_vec()),
                    ("two.txt".into(), b"2\n".to_vec()),
                ],
                _ => vec![
                    ("one.txt".into(), b"3\n".to_vec()),
                    ("two.txt".into(), b"2\n".to_vec()),
                ],
            };
            require(
                recorded_files_at(&repo, oid).unwrap() == expected,
                format!("history tree {index} must retain ordered intermediate/final bytes"),
            )?;
        }
        require(
            server.captured_requests().len() == 4,
            "three tool requests and one failure without retry",
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[
            ("three mutations then provider failure", &outcome.command),
            ("persistent Turn detail", &detail),
        ],
        server.captured_requests(),
        result,
    );
}
