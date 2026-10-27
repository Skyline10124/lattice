// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local workspace observation visibility and bounds (issue #46,
//! Codeberg #24, slice 2): proves all four `observe__workspace` modes share the
//! same Policy-Visible Workspace visibility and bounds through the built
//! binary.
//!
//! The fixture matrix covers tracked, untracked, ignored, explicitly-included
//! ignored, read-denied, system-excluded, and budget-exceeding paths. Each
//! tool call result is inspected in the **next captured provider request**
//! (the product feeds the full CEP result back to the LLM as a `tool` role
//! message); the JSONL stream only exposes result size / error summary.

mod support;

use std::fs;

use lattice_system_harness::{
    AssertError, ScenarioSpec, ScenarioWorld, SendExitCode, SessionSendDriver, SlotOutcome,
    SseScript, derive_send_exit_code,
};
use serde_json::{Value, json};
use support::{
    assert_clean_send, assert_completed_after_tools, driver, evidence, finish_scenario, require,
    tool_result_value, wire_fake_llm,
};

/// Build the observation fixture world with the full visibility matrix and
/// bounded observation policy.
fn observation_world(prefix: &str) -> (ScenarioWorld, ScenarioSpec) {
    let mut spec = ScenarioSpec::direct_local(prefix);
    // This scenario performs 20 sequential Tool calls and captures 22 provider
    // requests; its 29.80-second passing runtime leaves no useful margin under
    // the 30-second per-step default. Keep the 180-second total timeout.
    spec.step_timeout_secs = 60;
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
    let model = lattice_system_harness::ProviderFixture::chat_completions("dl-obs")
        .model_ref()
        .unwrap();
    let profile = format!(
        "\
schema_version = 1\n\n\
[agent]\nname = \"dl-obs\"\nmodel = \"{model}\"\n\n\
[prompt]\nsystem = \"You are a concise test agent.\"\n\n\
[tools]\nenabled = [\"observe__workspace\"]\n\n\
[lancet.workspace]\nincluded_ignored_read_patterns = [\"private/included.txt\"]\n\
denied_read_patterns = [\"denied/**\"]\n\n\
[lancet.observation]\n\
max_single_read_bytes = 32\n\
max_grep_matches = 2\n\
max_list_entries = 3\n\
max_find_paths = 3\n"
    );
    world
        .write_project_file(".lattice/agents/dl-obs/agent.toml", &profile)
        .expect("write profile");
    world
        .write_project_file(".lattice/private.txt", "needle LATTICE_SECRET\n")
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
        .write_project_file("denied/hidden.txt", "needle denied content\n")
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
    // Both full queries must exercise system exclusion for Git metadata too.
    world
        .write_project_file(".git/observation.txt", "needle git metadata\n")
        .expect("write system-excluded Git fixture");
    const OUTSIDE_CANARY: &str = "OUTSIDE_ROOT_SECRET_24_01\n";
    let outside_file = world.root().join("outside-observation.txt");
    fs::write(&outside_file, OUTSIDE_CANARY).expect("write outside-root canary");
    let outside_file_path = outside_file.to_string_lossy().into_owned();
    let outside_root_path = world.root().to_string_lossy().into_owned();
    let absolute_read =
        json!({"request": {"mode": "read_file", "path": outside_file_path}}).to_string();
    let absolute_list =
        json!({"request": {"mode": "list_directory", "path": outside_root_path}}).to_string();
    let absolute_grep = json!({"request": {
        "mode": "grep",
        "pattern": "OUTSIDE_ROOT",
        "path": outside_root_path,
    }})
    .to_string();
    let absolute_find = json!({"request": {
        "mode": "find_files",
        "pattern": "outside-observation.txt",
        "path": outside_root_path,
    }})
    .to_string();
    let calls: &[(&str, &str)] = &[
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"tracked.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"private/ignored.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"private/included.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"denied/hidden.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":".lattice/private.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"big.txt"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"list_directory","path":"."}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"grep","pattern":"needle","path":"."}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"find_files","pattern":"*.txt","path":"."}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"../outside-observation.txt"}}"#,
        ),
        ("observe__workspace", absolute_read.as_str()),
        (
            "observe__workspace",
            r#"{"request":{"mode":"list_directory","path":".."}}"#,
        ),
        ("observe__workspace", absolute_list.as_str()),
        (
            "observe__workspace",
            r#"{"request":{"mode":"grep","pattern":"OUTSIDE_ROOT","path":".."}}"#,
        ),
        ("observe__workspace", absolute_grep.as_str()),
        (
            "observe__workspace",
            r#"{"request":{"mode":"find_files","pattern":"outside-observation.txt","path":".."}}"#,
        ),
        ("observe__workspace", absolute_find.as_str()),
    ];
    let visibility_calls = [
        (
            "observe__workspace",
            r#"{"request":{"mode":"grep","pattern":"needle","path":"."}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"find_files","pattern":"*.txt","path":"."}}"#,
        ),
    ];
    let SseScript::Sequence { mut scripts } = tool_sequence(calls, "bounds complete") else {
        unreachable!("tool_sequence returns Sequence");
    };
    let SseScript::Sequence {
        scripts: visibility_scripts,
    } = tool_sequence(&visibility_calls, "visibility complete")
    else {
        unreachable!("tool_sequence returns Sequence");
    };
    scripts.extend(visibility_scripts);
    let server = wire_fake_llm(&world, "dl-obs", SseScript::Sequence { scripts });
    let mut scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-obs");
    let execution = send.send("observe the workspace").expect("turn drives");
    // Keep the original profile-bound truncation checks. A second Turn freezes
    // a larger bound so visibility is checked against the complete workspace.
    let profile_path = world
        .project_dir()
        .join(".lattice/agents/dl-obs/agent.toml");
    let profile = fs::read_to_string(&profile_path)
        .expect("read observation profile")
        .replace("max_grep_matches = 2", "max_grep_matches = 100")
        .replace("max_find_paths = 3", "max_find_paths = 100");
    fs::write(profile_path, profile).expect("raise visibility query bounds");
    let visibility = send
        .send("check complete workspace visibility")
        .expect("visibility Turn");
    let captures = server.captured_requests();
    let audit = send
        .run_command(&[
            "--json",
            "audit",
            "replay",
            "--session",
            send.session_id().unwrap(),
            "--turn",
            visibility.accepted_turn_id().unwrap(),
        ])
        .expect("read observation evidence");
    scenario_evidence.record_command("observation audit replay", &audit);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        let outcome = &execution.parsed;

        // 规范工具名与每次调用的结论都取自 Step 条目中的槽位。
        let slots = outcome.slots();
        require(
            slots.len() == 18 && slots.iter().all(|slot| slot.method == "observe__workspace"),
            format!("tool slot names mismatch: {slots:?}"),
        )?;

        // Calls 1-10 retain their visibility/bounds results; all 8 escapes fail.
        let ok_pattern: Vec<bool> = slots
            .iter()
            .map(|slot| slot.outcome == SlotOutcome::Succeeded)
            .collect();
        let expected_ok = [
            true, true, false, true, false, false, false, true, true, true, false, false, false,
            false, false, false, false, false,
        ];
        require(
            ok_pattern == expected_ok,
            format!("tool ok/fail pattern mismatch: {ok_pattern:?} expected {expected_ok:?}"),
        )?;

        // 结局取自最后一个 Step 条目，它在全部带调用的步骤之后。
        assert_completed_after_tools(outcome)?;

        // turn committed completed
        require(
            outcome.has_completed(),
            format!("turn did not complete: {:?}", outcome.type_sequence()),
        )?;
        let binary_exit = outcome.exit_code.expect("exit code classification");
        require(
            binary_exit == SendExitCode::Committed
                && derive_send_exit_code(&outcome.records) == SendExitCode::Committed,
            format!("exit mismatch: binary={binary_exit:?}"),
        )?;

        // --- captured tool result inspection ---
        // Bounds: 18 calls + plain text; visibility: 2 calls + plain text.
        require(
            captures.len() == 22,
            format!("expected 22 captures, got: {}", captures.len()),
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

        // 3. private/ignored.txt -> reported as missing, with no hidden reason
        let r3 = tool_result_value(&captures[3]).expect("ignored-path error envelope");
        require(
            r3.pointer("/error/code").and_then(Value::as_i64) == Some(-32003)
                && r3.pointer("/error/message").and_then(Value::as_str)
                    == Some("not_found: 'private/ignored.txt'")
                && !r3.to_string().contains("HiddenReason"),
            format!("ignored-path error mismatch: {r3}"),
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

        // 5. denied/hidden.txt -> reported as missing, with no policy detail
        let r5 = tool_result_value(&captures[5]).expect("denied-path error envelope");
        require(
            r5.pointer("/error/code").and_then(Value::as_i64) == Some(-32003)
                && r5.pointer("/error/message").and_then(Value::as_str)
                    == Some("not_found: 'denied/hidden.txt'")
                && !r5.to_string().contains("HiddenReason"),
            format!("denied-path error mismatch: {r5}"),
        )?;

        // 6. .lattice/private.txt -> system exclusion, reported as missing
        let r6 = tool_result_value(&captures[6]).expect("system-path error envelope");
        require(
            r6.pointer("/error/code").and_then(Value::as_i64) == Some(-32003)
                && r6.pointer("/error/message").and_then(Value::as_str)
                    == Some("not_found: '.lattice/private.txt'")
                && !r6.to_string().contains("HiddenReason"),
            format!("system-path error mismatch: {r6}"),
        )?;

        // 7. big.txt -> explicit bounded-read error
        let r7 = tool_result_value(&captures[7]).expect("read-limit error envelope");
        require(
            r7.pointer("/error/code").and_then(Value::as_i64) == Some(-32060)
                && r7.pointer("/error/message").and_then(Value::as_str)
                    == Some(
                        "limit observation.max_single_read_bytes exceeded: requested 53, allowed 32",
                    ),
            format!("read-limit error mismatch: {r7}"),
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

        // 9. grep "needle" "." -> truncated; visibility is checked in the next Turn.
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

        // 10. find_files "*.txt" "." -> truncated; visibility is checked in the next Turn.
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

        // Truncated walks can omit a leaked path. Check visibility only with
        // complete results against the fixture, independent of traversal order.
        // Tool results omit false truncation flags. Public audit preserves the
        // Host's explicit verdict, paired to each result by InvocationId.
        require(audit.is_clean_exit(0), "observation audit replay succeeds")?;
        let audit: Value =
            serde_json::from_slice(&audit.stdout).map_err(|e| AssertError::StdoutMissing {
                expected: format!("observation audit JSON: {e}"),
            })?;
        let calls = audit["calls"]
            .as_array()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "observation audit calls array".into(),
            })?;
        assert_clean_send(&visibility)?;
        assert_completed_after_tools(&visibility.parsed)?;
        let visibility_slots = visibility.slots();
        require(
            visibility_slots.len() == 2
                && visibility_slots.iter().all(|slot| {
                    slot.method == "observe__workspace" && slot.outcome == SlotOutcome::Succeeded
                }),
            "both complete visibility queries must succeed",
        )?;
        for (index, slot) in visibility_slots.iter().enumerate() {
            let invocation = &slot.invocation_id;
            let row = calls
                .iter()
                .find(|row| row["invocation_id"] == *invocation)
                .ok_or_else(|| AssertError::StdoutMissing {
                    expected: format!("audit row for visibility query {invocation}"),
                })?;
            let effects = row
                .pointer("/state/evidence/value/effects/effects")
                .and_then(Value::as_array)
                .ok_or_else(|| AssertError::StdoutMissing {
                    expected: format!("Host effects for visibility query {invocation}"),
                })?;
            let observations: Vec<_> = effects
                .iter()
                .filter(|effect| effect["source"] == "operation")
                .collect();
            require(
                observations.len() == 1
                    && observations[0]["target"] == json!({"kind":"path","path":"."})
                    && observations[0]["truncated"].as_bool() == Some(false),
                format!(
                    "visibility query {index} must have explicit untruncated Host evidence: {observations:?}"
                ),
            )?;
        }
        let complete_grep = tool_result_value(&captures[20]).expect("complete grep result");
        require(
            complete_grep["mode"] == "grep",
            format!("complete grep mode mismatch: {complete_grep}"),
        )?;
        let matches = complete_grep["result"]["matches"]
            .as_array()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "complete grep matches array".into(),
            })?;
        let mut actual_matches = matches
            .iter()
            .map(|entry| {
                let path = entry["path"]
                    .as_str()
                    .ok_or_else(|| AssertError::StdoutMissing {
                        expected: "complete grep match path".into(),
                    })?;
                let line = entry["line"]
                    .as_u64()
                    .ok_or_else(|| AssertError::StdoutMissing {
                        expected: "complete grep match line".into(),
                    })?;
                let text = entry["text"]
                    .as_str()
                    .ok_or_else(|| AssertError::StdoutMissing {
                        expected: "complete grep match text".into(),
                    })?;
                Ok((path, line, text))
            })
            .collect::<Result<Vec<_>, AssertError>>()?;
        actual_matches.sort_unstable();
        let expected_matches = [
            ("g1.txt", 1, "needle one"),
            ("g2.txt", 1, "needle two"),
            ("g3.txt", 1, "needle three"),
        ];
        require(
            actual_matches == expected_matches,
            format!(
                "complete grep exact fixture matches: expected {expected_matches:?}, got {actual_matches:?}"
            ),
        )?;
        let complete_find = tool_result_value(&captures[21]).expect("complete find result");
        require(
            complete_find["mode"] == "find_files",
            format!("complete find mode mismatch: {complete_find}"),
        )?;
        let paths = complete_find["result"]["paths"].as_array().ok_or_else(|| {
            AssertError::StdoutMissing {
                expected: "complete find paths array".into(),
            }
        })?;
        let mut actual_paths = paths
            .iter()
            .map(|path| {
                path.as_str().ok_or_else(|| AssertError::StdoutMissing {
                    expected: "complete find path string".into(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        actual_paths.sort_unstable();
        let expected_paths = [
            "big.txt",
            "g1.txt",
            "g2.txt",
            "g3.txt",
            "notes.txt",
            "private/included.txt",
            "tracked.txt",
        ];
        require(
            actual_paths == expected_paths,
            format!(
                "complete find exact fixture paths: expected {expected_paths:?}, got {actual_paths:?}"
            ),
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
            let error = tool_result_value(capture)
                .unwrap_or_else(|| panic!("root escape missing error envelope for {path:?}"));
            let expected = if *traversal {
                format!("path contains '..' traversal: '{path}'")
            } else {
                format!("path must be relative: '{path}'")
            };
            require(
                error.pointer("/error/code").and_then(Value::as_i64) == Some(-32002)
                    && error.pointer("/error/message").and_then(Value::as_str)
                        == Some(expected.as_str()),
                format!("root-escape error mismatch for {path:?}: {error}"),
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
        // 每次越界尝试都以 `PathContainment` 拒绝，Step 条目的槽位给出同一结论。
        //
        // Cell admission precedes the Tool Host's path resolver. Lexical or
        // symlink escape therefore keeps the precise `PathContainment` code.
        let containment_denied_finishes = outcome
            .slots()
            .iter()
            .filter(|slot| {
                slot.outcome == SlotOutcome::Failed
                    && slot.error_code.as_deref() == Some("path_containment")
            })
            .count();
        require(
            containment_denied_finishes == 8,
            format!(
                "expected 8 PathContainment slots, got \
                 {containment_denied_finishes}: {:?}",
                outcome.slots()
            ),
        )?;

        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("observation turn", &execution.command),
            ("visibility turn", &visibility.command),
        ],
        captures,
        result,
    );
}

/// 静态符号链接覆盖真实的 Component/Host/provider 接线；确定性的检查/打开
/// 竞态留在 `LocalWorkspace` 单元测试中。
#[test]
fn direct_observation_symlinks_are_refused_without_canary_leak() {
    let driver = driver();
    let (world, spec) = observation_world("direct-observation-symlinks");
    const CANARY: &str = "I233_E2E_EXTERNAL_CANARY_49da\n";
    const NAME: &str = "I233_E2E_EXTERNAL_NAME_49da.txt";
    let outside = world.root().join("outside-symlinks");
    fs::create_dir(&outside).expect("outside directory");
    let canary_file = outside.join(NAME);
    fs::write(&canary_file, CANARY).expect("outside canary");
    std::os::unix::fs::symlink(&canary_file, world.project_dir().join("file-link"))
        .expect("file symlink");
    std::os::unix::fs::symlink(&outside, world.project_dir().join("dir-link"))
        .expect("directory symlink");
    let calls = [
        (
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"file-link"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"list_directory","path":"dir-link"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"grep","pattern":"I233_E2E_EXTERNAL_CANARY","path":"dir-link"}}"#,
        ),
        (
            "observe__workspace",
            r#"{"request":{"mode":"find_files","pattern":"*","path":"dir-link"}}"#,
        ),
    ];
    let server = wire_fake_llm(&world, "dl-obs", tool_sequence(&calls, "symlinks refused"));
    let scenario_evidence = evidence(&world, &spec);
    let execution = SessionSendDriver::new(&driver, &world, &spec, "dl-obs")
        .send("observe symlink paths")
        .expect("turn drives");
    let captures = server.captured_requests();
    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        require(
            captures.len() == 5,
            "expected four symlink results and final provider request",
        )?;
        let finishes: Vec<bool> = execution
            .slots()
            .iter()
            .map(|slot| slot.outcome == SlotOutcome::Succeeded)
            .collect();
        require(
            finishes == [false; 4],
            format!("symlink calls were not all refused: {finishes:?}"),
        )?;
        for (index, path) in ["file-link", "dir-link", "dir-link", "dir-link"]
            .iter()
            .enumerate()
        {
            let error = tool_result_value(&captures[index + 1]).expect("symlink error envelope");
            require(
                error.pointer("/error/code").and_then(Value::as_i64) == Some(-32002),
                format!("symlink error code mismatch: {error}"),
            )?;
            require(
                error.pointer("/error/message").and_then(Value::as_str)
                    == Some(format!("path escapes the project root: '{path}'").as_str()),
                format!("symlink error message mismatch: {error}"),
            )?;
        }
        let containment = execution
            .slots()
            .iter()
            .filter(|slot| {
                slot.outcome == SlotOutcome::Failed
                    && slot.error_code.as_deref() == Some("path_containment")
            })
            .count();
        require(
            containment == 4,
            format!("expected four PathContainment finishes, got {containment}"),
        )?;
        require(
            captures.iter().all(|capture| {
                let raw = capture.raw.to_string();
                !raw.contains(CANARY.trim()) && !raw.contains(NAME)
            }),
            "outside canary leaked into provider requests",
        )?;
        require(
            fs::read_to_string(&canary_file).expect("read canary") == CANARY
                && fs::read_dir(&outside).expect("outside entries").count() == 1,
            "outside objects changed during observation denials",
        )?;
        Ok(())
    })();
    finish_scenario(
        scenario_evidence,
        &[("symlink observations", &execution.command)],
        captures,
        result,
    );
}
