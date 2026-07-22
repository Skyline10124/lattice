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
//!
//! ponytail: helpers duplicate `tests/direct_local_turn.rs`; the planned
//! later-slice split owns the dedupe.

#![cfg(unix)]

use lattice_system_harness::{
    AssertError, CapturedRequest, CommandOutcome, FAKE_API_KEY, LatticeDriver, ScenarioEvidence,
    ScenarioSpec, ScenarioWorld, SendExecution, SendExitCode, SessionSendDriver, SseScript,
    Termination, TurnEventLine, assert_event_subsequence, derive_send_exit_code, failure_dir,
};
use serde_json::Value;

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

fn assert_clean_send(execution: &SendExecution) -> Result<(), AssertError> {
    if !matches!(&execution.command.termination, Termination::Exited(Some(_))) {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send did not exit normally: {:?}",
                execution.command.termination
            ),
        });
    }
    if !execution.malformed_lines.is_empty() {
        return Err(AssertError::StdoutMissing {
            expected: format!(
                "session send emitted malformed JSONL: {:?}",
                execution.malformed_lines
            ),
        });
    }
    Ok(())
}

fn require(condition: bool, expected: impl Into<String>) -> Result<(), AssertError> {
    if condition {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: expected.into(),
        })
    }
}

fn evidence(world: &ScenarioWorld, spec: &ScenarioSpec) -> ScenarioEvidence {
    ScenarioEvidence::new(world, failure_dir(&spec.name), [FAKE_API_KEY.to_string()])
        .expect("scenario evidence")
}

fn finish_scenario(
    mut evidence: ScenarioEvidence,
    commands: &[(&str, &CommandOutcome)],
    captures: Vec<CapturedRequest>,
    result: Result<(), AssertError>,
) {
    for (summary, outcome) in commands {
        evidence.record_command(*summary, outcome);
    }
    evidence.record_captures(captures);
    evidence
        .finish(result.is_err())
        .unwrap_or_else(|error| panic!("scenario evidence failed: {error}"));
    result.unwrap_or_else(|error| panic!("{error}"));
}

fn wire_fake_llm(
    world: &mut ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> lattice_system_harness::FakeLlmServer {
    let server = lattice_system_harness::FakeLlmServer::start(script).expect("start fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );
    server
}

/// The content of the last `tool` role message in a captured request, or
/// `None` if there is no tool message. Successful `observe.workspace` calls
/// produce a JSON string starting with `{`; denials produce a plain error
/// string.
fn last_tool_content(capture: &CapturedRequest) -> Option<String> {
    capture
        .raw
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| {
            msgs.iter()
                .rev()
                .find(|m| m.get("role").and_then(Value::as_str) == Some("tool"))
                .and_then(|m| m.get("content").and_then(Value::as_str))
                .map(str::to_string)
        })
}

/// Parsed `observe.workspace` result JSON from the last tool message, or
/// `None` if the tool call failed (error string) or no tool message exists.
fn tool_result_value(capture: &CapturedRequest) -> Option<Value> {
    let content = last_tool_content(capture)?;
    if content.starts_with('{') {
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

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
    let (mut world, spec) = observation_world("direct-observation");
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
    ];
    let server = wire_fake_llm(
        &mut world,
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
            tool_starts.len() == 10 && tool_starts.iter().all(|n| *n == "observe.workspace"),
            format!("tool lifecycle names mismatch: {tool_starts:?}"),
        )?;

        // ok/fail pattern: calls 1-2 ok, 3 fail, 4 ok, 5-7 fail, 8-10 ok
        let ok_pattern: Vec<bool> = outcome
            .events
            .iter()
            .filter_map(|e| match e {
                TurnEventLine::ToolCallFinished { ok, .. } => Some(*ok),
                _ => None,
            })
            .collect();
        let expected_ok = [
            true, true, false, true, false, false, false, true, true, true,
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
        // 10 tool calls + 1 plain text = 11 captures; result_i in captures[i].
        require(
            captures.len() == 11,
            format!("expected 11 captures, got: {}", captures.len()),
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

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observation turn", &execution.command)],
        captures,
        result,
    );
}
