// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local workspace observation visibility and bounds (issue #24
//! slice 2): proves all four `observe.workspace` modes share the same
//! Policy-Visible Workspace visibility and bounds through the built binary.
//!
//! The fixture matrix covers tracked, untracked, ignored, explicitly-included
//! ignored, read-denied, system-excluded, and budget-exceeding paths. Each
//! tool call result is inspected in the **next captured provider request**
//! (the product feeds the full CEP result back to the LLM as a `tool` role
//! message); the JSONL stream only exposes result size / error summary.

#![cfg(unix)]

mod support;

use std::fs;

use lattice_system_harness::{
    AssertError, ScenarioSpec, ScenarioWorld, SendExitCode, SessionSendDriver, SseScript,
    TurnEventLine, assert_event_subsequence, derive_send_exit_code,
};
use serde_json::{Value, json};
use support::{
    assert_clean_send, driver, evidence, finish_scenario, last_tool_content, require,
    tool_result_value, wire_fake_llm,
};

/// Build the observation fixture world with the full visibility matrix and
/// bounded observation policy.
fn observation_world(prefix: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");

    // Tracked files (committed by init_git_project).
    world
        .write_project_file(".gitignore", "private/\n")
        .expect("write .gitignore");
    world
        .write_project_file("src/lib.rs", "fn main() {}\n")
        .expect("write src/lib.rs");
    world
        .write_project_file("tracked.txt", "small tracked content\n")
        .expect("write tracked.txt");
    world
        .write_project_file("g1.txt", "needle one\n")
        .expect("write g1.txt");
    world
        .write_project_file("g2.txt", "needle two\n")
        .expect("write g2.txt");
    world
        .write_project_file("g3.txt", "needle three\n")
        .expect("write g3.txt");
    world
        .write_project_file(
            "big.txt",
            "this file is deliberately over thirty-two bytes long\n",
        )
        .expect("write big.txt");

    // Agent profile + LATTICE private state (system-excluded).
    let profile = "\
schema_version = 1\n\n\
[agent]\nname = \"dl-obs\"\nmodel = \"compat:dl-obs\"\n\n\
[prompt]\nsystem = \"You are a concise test agent.\"\n\n\
[tools]\nenabled = [\"observe.workspace\"]\n\n\
[lancet.workspace]\nincluded_ignored_read_patterns = [\"private/included.txt\"]\n\
denied_read_patterns = [\"denied/**\"]\n\n\
[lancet.observation]\n\
max_single_read_bytes = 32\n\
max_grep_matches = 2\n\
max_list_entries = 3\n\
max_find_paths = 3\n";
    world
        .write_project_file(".lattice/agents/dl-obs/agent.toml", profile)
        .expect("write profile");
    world
        .write_project_file(".lattice/private.txt", "LATTICE_SECRET\n")
        .expect("write .lattice/private.txt");

    world.init_git_project().expect("init git project");

    // Untracked files (written after init so they are not in the git tree).
    world
        .write_project_file("notes.txt", "untracked note\n")
        .expect("write notes.txt");
    world
        .write_project_file("private/ignored.txt", "needle hidden\n")
        .expect("write private/ignored.txt");
    world
        .write_project_file("private/included.txt", "included content\n")
        .expect("write private/included.txt");
    world
        .write_project_file("denied/hidden.txt", "denied content\n")
        .expect("write denied/hidden.txt");

    (world, spec)
}

/// Build a sequence of tool-call scripts followed by a plain-text terminator.
fn tool_sequence(calls: &[(&str, &str)], terminator: &str) -> SseScript {
    let mut scripts: Vec<SseScript> = calls
        .iter()
        .enumerate()
        .map(|(i, (tool, args))| SseScript::ToolCall {
            call_id: format!("call_{}", i + 1),
            tool_name: tool.to_string(),
            arguments_json: args.to_string(),
        })
        .collect();
    scripts.push(SseScript::plain(terminator));
    SseScript::Sequence { scripts }
}

/// One comprehensive observation scenario: visibility matrix + bounds +
/// four-mode agreement + lifecycle ordering.
#[test]
fn direct_observation_visibility_and_bounds() {
    let driver = driver();
    let (world, spec) = observation_world("direct-observation");
    const OUTSIDE_CANARY: &str = "OUTSIDE_ROOT_SECRET_24_01\n";
    let outside_file = world.root().join("outside-observation.txt");
    fs::write(&outside_file, OUTSIDE_CANARY).expect("write outside-root canary");
    let outside_file_path = outside_file.to_string_lossy().into_owned();
    let outside_root_path = world.root().to_string_lossy().into_owned();
    let absolute_read = json!({"mode": "read_file", "path": outside_file_path}).to_string();
    let absolute_list = json!({"mode": "list_directory", "path": outside_root_path}).to_string();
    let absolute_grep = json!({
        "mode": "grep",
        "pattern": "OUTSIDE_ROOT",
        "path": outside_root_path,
    })
    .to_string();
    let absolute_find = json!({
        "mode": "find_files",
        "pattern": "outside-observation.txt",
        "path": outside_root_path,
    })
    .to_string();
    let calls: &[(&str, &str)] = &[
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"tracked.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"notes.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"private/ignored.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"private/included.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"denied/hidden.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":".lattice/private.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"big.txt"}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"list_directory","path":"."}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"grep","pattern":"needle","path":"."}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"find_files","pattern":"*.txt","path":"."}"#,
        ),
        (
            "observe.workspace",
            r#"{"mode":"read_file","path":"../outside-observation.txt"}"#,
        ),
        ("observe.workspace", absolute_read.as_str()),
        (
            "observe.workspace",
            r#"{"mode":"list_directory","path":".."}"#,
        ),
        ("observe.workspace", absolute_list.as_str()),
        (
            "observe.workspace",
            r#"{"mode":"grep","pattern":"OUTSIDE_ROOT","path":".."}"#,
        ),
        ("observe.workspace", absolute_grep.as_str()),
        (
            "observe.workspace",
            r#"{"mode":"find_files","pattern":"outside-observation.txt","path":".."}"#,
        ),
        ("observe.workspace", absolute_find.as_str()),
    ];
    let server = wire_fake_llm(
        &world,
        "dl-obs",
        tool_sequence(calls, "observation complete"),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-obs", "main").expect("create session");
    let execution = send.send("observe the workspace").expect("turn drives");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        let outcome = &execution.parsed;

        // --- canonical tool name and lifecycle ordering ---
        let tool_starts: Vec<&str> = outcome
            .events
            .iter()
            .filter_map(|e| match e {
                TurnEventLine::ToolCallStarted { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        require(
            tool_starts.len() == 18 && tool_starts.iter().all(|n| *n == "observe.workspace"),
            format!("tool lifecycle names mismatch: {tool_starts:?}"),
        )?;

        // Calls 1-10 retain their visibility/bounds results; all 8 escapes fail.
        let ok_pattern: Vec<bool> = outcome
            .events
            .iter()
            .filter_map(|e| match e {
                TurnEventLine::ToolCallFinished { ok, .. } => Some(*ok),
                _ => None,
            })
            .collect();
        let expected_ok = [
            true, true, false, true, false, false, false, true, true, true, false, false, false,
            false, false, false, false, false,
        ];
        require(
            ok_pattern == expected_ok,
            format!("tool ok/fail pattern mismatch: {ok_pattern:?} expected {expected_ok:?}"),
        )?;

        // tool activity and CEP lifecycle precede commit
        assert_event_subsequence(outcome, &["tool_call_started", "turn_committed"])
            .map_err(|e| AssertError::StdoutMissing { expected: e })?;

        // turn committed completed
        require(
            outcome.has_committed_completed(),
            format!("turn did not complete: {:?}", outcome.type_sequence()),
        )?;
        let binary_exit = outcome.exit_code.expect("exit code classification");
        require(
            binary_exit == SendExitCode::Committed
                && derive_send_exit_code(&outcome.events) == SendExitCode::Committed,
            format!("exit mismatch: binary={binary_exit:?}"),
        )?;

        // --- captured tool result inspection ---
        // 18 tool calls + 1 plain text = 19 captures; result_i in captures[i].
        require(
            captures.len() == 19,
            format!("expected 19 captures, got: {}", captures.len()),
        )?;

        // 1. tracked.txt -> visible, content present
        let r1 = tool_result_value(&captures[1]).expect("tracked.txt result");
        require(
            r1.get("mode").and_then(Value::as_str) == Some("read_file")
                && r1["result"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("small tracked content"),
            format!("tracked.txt result mismatch: {r1}"),
        )?;

        // 2. notes.txt (untracked, non-ignored) -> visible
        let r2 = tool_result_value(&captures[2]).expect("notes.txt result");
        require(
            r2["result"]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("untracked note"),
            format!("notes.txt result mismatch: {r2}"),
        )?;

        // 3. private/ignored.txt -> denied (no JSON result)
        require(
            tool_result_value(&captures[3]).is_none(),
            format!(
                "ignored path should not produce a JSON result: {:?}",
                last_tool_content(&captures[3])
            ),
        )?;

        // 4. private/included.txt -> visible (explicitly included)
        let r4 = tool_result_value(&captures[4]).expect("included.txt result");
        require(
            r4["result"]["content"]
                .as_str()
                .unwrap_or_default()
                .contains("included content"),
            format!("included.txt result mismatch: {r4}"),
        )?;

        // 5. denied/hidden.txt -> denied (read denial)
        require(
            tool_result_value(&captures[5]).is_none(),
            "denied-read path should not produce a JSON result",
        )?;

        // 6. .lattice/private.txt -> denied (system exclusion)
        require(
            tool_result_value(&captures[6]).is_none(),
            ".lattice path should not produce a JSON result (system exclusion)",
        )?;

        // 7. big.txt -> denied (single-read budget exceeded)
        require(
            tool_result_value(&captures[7]).is_none(),
            "big.txt should not produce a JSON result (budget exceeded)",
        )?;

        // 8. list_directory "." -> truncated
        let r8 = tool_result_value(&captures[8]).expect("list_directory result");
        require(
            r8.get("mode").and_then(Value::as_str) == Some("list_directory"),
            format!("list_directory mode mismatch: {r8}"),
        )?;
        let entries = r8["result"]["entries"].as_array().expect("entries array");
        require(
            entries.len() <= 3 && r8["result"]["truncated"].as_bool() == Some(true),
            format!("list_directory not truncated to 3: {r8}"),
        )?;
        // system-excluded and ignored names absent from listing
        let names: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.get("name").and_then(Value::as_str))
            .collect();
        require(
            !names.iter().any(|n| *n == ".lattice" || *n == ".git"),
            format!("system-excluded dirs in listing: {names:?}"),
        )?;

        // 9. grep "needle" "." -> truncated, no hidden paths
        let r9 = tool_result_value(&captures[9]).expect("grep result");
        require(
            r9.get("mode").and_then(Value::as_str) == Some("grep"),
            format!("grep mode mismatch: {r9}"),
        )?;
        let matches = r9["result"]["matches"].as_array().expect("matches array");
        require(
            matches.len() <= 2 && r9["result"]["truncated"].as_bool() == Some(true),
            format!("grep not truncated to 2: {r9}"),
        )?;
        let match_paths: Vec<&str> = matches
            .iter()
            .filter_map(|m| m.get("path").and_then(Value::as_str))
            .collect();
        require(
            !match_paths
                .iter()
                .any(|p| p.contains("private/ignored") || p.contains("denied/")),
            format!("grep returned hidden/denied paths: {match_paths:?}"),
        )?;

        // 10. find_files "*.txt" "." -> truncated, no hidden paths
        let r10 = tool_result_value(&captures[10]).expect("find_files result");
        require(
            r10.get("mode").and_then(Value::as_str) == Some("find_files"),
            format!("find_files mode mismatch: {r10}"),
        )?;
        let paths = r10["result"]["paths"].as_array().expect("paths array");
        require(
            paths.len() <= 3 && r10["result"]["truncated"].as_bool() == Some(true),
            format!("find_files not truncated to 3: {r10}"),
        )?;
        let path_strs: Vec<&str> = paths.iter().filter_map(Value::as_str).collect();
        require(
            !path_strs
                .iter()
                .any(|p| p.contains("private/ignored") || p.contains("denied/")),
            format!("find_files returned hidden/denied paths: {path_strs:?}"),
        )?;

        // 11-18. Every observation mode rejects traversal and absolute paths.
        let escape_paths = [
            ("../outside-observation.txt", true),
            (outside_file_path.as_str(), false),
            ("..", true),
            (outside_root_path.as_str(), false),
            ("..", true),
            (outside_root_path.as_str(), false),
            ("..", true),
            (outside_root_path.as_str(), false),
        ];
        for (offset, (path, traversal)) in escape_paths.iter().enumerate() {
            let capture = &captures[11 + offset];
            require(
                tool_result_value(capture).is_none(),
                format!("root escape unexpectedly returned JSON for {path:?}"),
            )?;
            let expected = if *traversal {
                format!("path contains '..' traversal: '{path}'")
            } else {
                format!("path must be relative: '{path}'")
            };
            require(
                last_tool_content(capture).as_deref() == Some(expected.as_str()),
                format!(
                    "root-escape error mismatch for {path:?}: {:?}",
                    last_tool_content(capture)
                ),
            )?;
        }
        require(
            captures
                .iter()
                .all(|capture| !capture.raw.to_string().contains(OUTSIDE_CANARY.trim())),
            "outside-root canary leaked into provider captures",
        )?;
        require(
            fs::read_to_string(&outside_file).unwrap_or_default() == OUTSIDE_CANARY,
            "outside-root file changed during observation denials",
        )?;
        let guard_denied_finishes = outcome
            .events
            .iter()
            .filter(|event| match event {
                TurnEventLine::Unknown { type_name, raw } if type_name == "cep_call_finished" => {
                    raw.get("ok").and_then(Value::as_bool) == Some(false)
                        && raw.get("error_code").and_then(Value::as_str) == Some("GuardDenied")
                }
                _ => false,
            })
            .count();
        require(
            guard_denied_finishes == 8,
            format!(
                "expected 8 GuardDenied CEP finishes, got {guard_denied_finishes}: {:?}",
                outcome.events
            ),
        )?;

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observation turn", &execution.command)],
        captures,
        result,
    );
}
