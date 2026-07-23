// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local atomic mutation, external concurrency, and effect denial
//! (issue #24 slice 3).
//!
//! Covers `mutate.create` / `mutate.edit` operating on current Local state:
//! success paths leave bytes immediately; denials and stale preconditions
//! never change the target; a later provider failure does not roll back a
//! successful mutation; preflight effect-kind denial never calls the backend.
//!
//! Tool results are inspected in the next captured provider request (the
//! product feeds the CEP result back as a `tool` role message).
//!
//! ponytail: helpers duplicate other Direct-Local tests; later-slice split
//! owns the dedupe.

#![cfg(unix)]

use std::fs;
use std::time::Duration;

use lattice_system_harness::{
    assert_event_subsequence, failure_dir, AssertError, CapturedRequest, CommandOutcome,
    LatticeDriver, ScenarioEvidence, ScenarioSpec, ScenarioWorld, SendExecution, SendExitCode,
    SessionSendDriver, SseScript, Termination, TurnEventLine, FAKE_API_KEY,
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
    world: &ScenarioWorld,
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

fn wire_gated_fake_llm(
    world: &ScenarioWorld,
    agent: &str,
    script: SseScript,
) -> (
    lattice_system_harness::FakeLlmServer,
    lattice_system_harness::ResponseGate,
) {
    let (server, gate) =
        lattice_system_harness::FakeLlmServer::start_gated(script).expect("start gated fake llm");
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", server.base_url());
    world.set_env("OPENAI_COMPATIBLE_MODEL", agent);
    world.set_env(
        "OPENAI_COMPATIBLE_API_KEY",
        lattice_system_harness::FAKE_API_KEY,
    );
    (server, gate)
}

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

fn tool_result_value(capture: &CapturedRequest) -> Option<Value> {
    let content = last_tool_content(capture)?;
    if content.starts_with('{') {
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

fn tool_failed(outcome: &lattice_system_harness::SendOutcome, name: &str) -> bool {
    outcome.events.iter().any(|event| {
        matches!(
            event,
            TurnEventLine::ToolCallFinished {
                name: n,
                ok: false,
                ..
            } if n == name
        )
    })
}

fn tool_ok(outcome: &lattice_system_harness::SendOutcome, name: &str) -> bool {
    outcome.events.iter().any(|event| {
        matches!(
            event,
            TurnEventLine::ToolCallFinished {
                name: n,
                ok: true,
                ..
            } if n == name
        )
    })
}

fn is_non_completed_terminal(outcome: &lattice_system_harness::SendOutcome) -> bool {
    outcome.events.iter().any(|e| match e {
        TurnEventLine::TurnFailed { .. }
        | TurnEventLine::TurnCancelled { .. }
        | TurnEventLine::CommitFailed { .. } => true,
        TurnEventLine::TurnCommitted { status, .. } => status != "completed",
        _ => false,
    }) || outcome.exit_code == Some(SendExitCode::Failed)
}

fn mutation_world(
    prefix: &str,
    agent: &str,
    tools: &str,
    extra_profile: &str,
) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n\
         [agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n\
         [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
         [tools]\nenabled = {tools}\n{extra_profile}"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git");
    (world, spec)
}

fn tool_call(id: &str, name: &str, args: &str) -> SseScript {
    SseScript::ToolCall {
        call_id: id.into(),
        tool_name: name.into(),
        arguments_json: args.into(),
    }
}

fn sequence(scripts: Vec<SseScript>) -> SseScript {
    SseScript::Sequence { scripts }
}

/// `mutate.create` succeeds only while the target is absent; bytes appear
/// immediately. A second create of the same path fails and preserves bytes.
#[test]
fn direct_mutation_create_atomic_and_existing_target() {
    let driver = driver();
    let (world, spec) = mutation_world("direct-mut-create", "dl-create", "[\"mutate.create\"]", "");
    let server = wire_fake_llm(
        &world,
        "dl-create",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"new.txt","content":"created-v1\n","create_parents":false}"#,
            ),
            SseScript::plain("created"),
            tool_call(
                "c2",
                "mutate.create",
                r#"{"path":"new.txt","content":"created-v2\n","create_parents":false}"#,
            ),
            SseScript::plain("second create failed as expected"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-create", "main").expect("session");

    let first = send.send("create new.txt").expect("first turn");
    let second = send.send("create new.txt again").expect("second turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(tool_ok(&first.parsed, "mutate.create"), "first create ok")?;
        require(
            first.has_committed_completed(),
            format!("first turn incomplete: {:?}", first.type_sequence()),
        )?;
        let path = world.project_dir().join("new.txt");
        require(path.is_file(), "create did not write new.txt")?;
        require(
            fs::read_to_string(&path).unwrap_or_default() == "created-v1\n",
            format!("create bytes wrong: {:?}", fs::read(&path)),
        )?;

        require(
            tool_failed(&second.parsed, "mutate.create"),
            "second create of existing target must fail",
        )?;
        require(
            second.has_committed_completed(),
            "second turn should still complete after tool failure",
        )?;
        require(
            fs::read_to_string(&path).unwrap_or_default() == "created-v1\n",
            "failed create must preserve existing bytes",
        )?;

        // Successful create result JSON in next provider request.
        require(captures.len() >= 2, "expected create + recovery captures")?;
        let created = tool_result_value(&captures[1]).expect("create result JSON");
        require(
            created.get("path").and_then(Value::as_str) == Some("new.txt")
                || created.pointer("/result/path").and_then(Value::as_str) == Some("new.txt")
                || created.to_string().contains("new.txt"),
            format!("create result missing path: {created}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("create success", &first.command),
            ("create existing fails", &second.command),
        ],
        captures,
        result,
    );
}

/// `mutate.edit` with a known Local Revision succeeds; a nonmatching anchor
/// fails before install and preserves bytes.
#[test]
fn direct_mutation_edit_hash_and_anchor() {
    let driver = driver();
    let (world, spec) = mutation_world(
        "direct-mut-edit",
        "dl-edit",
        "[\"observe.workspace\", \"mutate.edit\"]",
        "",
    );
    world
        .write_project_file("tracked.txt", "hello world\n")
        .expect("seed file");
    world.commit_all("seed tracked").expect("commit seed");

    let server = wire_fake_llm(
        &world,
        "dl-edit",
        sequence(vec![
            // Round 1: observe to learn content_hash
            tool_call(
                "o1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"tracked.txt"}"#,
            ),
            // Round 2: edit with the hash from the previous tool message —
            // but the fake LLM cannot dynamically read captures. We drive
            // two separate turns instead: observe turn, then edit turn with
            // hash extracted from captures.
            SseScript::plain("observed"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-edit", "main").expect("session");

    let observe = send.send("read tracked.txt").expect("observe turn");
    let captures_obs = server.captured_requests();
    let after_observe =
        fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default();
    assert_eq!(
        after_observe, "hello world\n",
        "file changed during observe: {after_observe:?}"
    );

    // Shut down first server; we'll spin a second one for edit with the hash.
    drop(server);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&observe)?;
        require(tool_ok(&observe.parsed, "observe.workspace"), "observe ok")?;
        require(captures_obs.len() >= 2, "observe needs tool result capture")?;
        let observed = tool_result_value(&captures_obs[1]).expect("observe result");
        let hash = observed
            .pointer("/result/content_hash")
            .and_then(Value::as_str)
            .or_else(|| observed.get("content_hash").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        require(
            hash.starts_with("blake3:") && hash.len() == "blake3:".len() + 64,
            format!("bad content_hash from observe: {observed}"),
        )?;

        let good_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"hello world","new":"hello lattice"}}]}}"#
        );
        let bad_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"NO_SUCH_ANCHOR","new":"x"}}]}}"#
        );

        // After good edit the hash changes; for the bad-anchor case we need
        // the post-good hash OR we run bad-anchor first on the original file.
        // Order: bad-anchor first (preserves bytes + original hash), then good edit.
        let server2 = wire_fake_llm(
            &world,
            "dl-edit",
            sequence(vec![
                tool_call("e_bad", "mutate.edit", &bad_args),
                SseScript::plain("bad anchor denied"),
                tool_call("e_good", "mutate.edit", &good_args),
                SseScript::plain("edited"),
            ]),
        );

        let bad = send.send("edit with bad anchor").expect("bad edit turn");
        assert_clean_send(&bad)?;
        require(
            tool_failed(&bad.parsed, "mutate.edit"),
            format!(
                "nonmatching anchor must fail; events={:?}",
                bad.type_sequence()
            ),
        )?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "hello world\n",
            "failed edit must preserve original bytes",
        )?;
        require(bad.has_committed_completed(), "bad-edit turn completes")?;

        let good = send.send("edit with good anchor").expect("good edit turn");
        let captures_edit = server2.captured_requests();

        assert_clean_send(&good)?;
        require(tool_ok(&good.parsed, "mutate.edit"), "good edit ok")?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "hello lattice\n",
            "good edit must install new bytes",
        )?;
        require(good.has_committed_completed(), "good-edit turn completes")?;
        require(
            captures_edit.len() >= 2,
            format!("edit captures missing: {}", captures_edit.len()),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observe for hash", &observe.command)],
        captures_obs,
        result,
    );
}

/// External writer changes the file between observe and edit; stale edit
/// returns failure and preserves external bytes. External writes between
/// Tool calls are visible to the next observe.
#[test]
fn direct_mutation_stale_edit_and_external_visibility() {
    let driver = driver();
    let (world, spec) = mutation_world(
        "direct-mut-stale",
        "dl-stale",
        "[\"observe.workspace\", \"mutate.edit\"]",
        "",
    );
    world
        .write_project_file("tracked.txt", "original\n")
        .expect("seed");
    world.commit_all("seed").expect("commit");

    let server = wire_fake_llm(
        &world,
        "dl-stale",
        sequence(vec![
            tool_call(
                "o1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"tracked.txt"}"#,
            ),
            SseScript::plain("observed original"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-stale", "main").expect("session");
    let observe = send.send("read tracked").expect("observe");
    let captures_obs = server.captured_requests();
    drop(server);

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&observe)?;
        let observed = tool_result_value(&captures_obs[1]).expect("observe result");
        let hash = observed
            .pointer("/result/content_hash")
            .and_then(Value::as_str)
            .or_else(|| observed.get("content_hash").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        require(
            hash.starts_with("blake3:"),
            format!("hash missing: {observed}"),
        )?;

        world
            .write_project_file("tracked.txt", "external\n")
            .expect("external write");

        let stale_args = format!(
            r#"{{"path":"tracked.txt","expected_hash":"{hash}","edits":[{{"op":"replace_text","old":"original","new":"agent"}}]}}"#
        );
        let server2 = wire_fake_llm(
            &world,
            "dl-stale",
            sequence(vec![
                tool_call("e_stale", "mutate.edit", &stale_args),
                SseScript::plain("stale denied"),
                tool_call(
                    "o2",
                    "observe.workspace",
                    r#"{"mode":"read_file","path":"tracked.txt"}"#,
                ),
                SseScript::plain("see external"),
            ]),
        );

        let stale = send.send("edit with stale hash").expect("stale edit");
        assert_clean_send(&stale)?;
        require(
            tool_failed(&stale.parsed, "mutate.edit"),
            "stale edit must fail",
        )?;
        require(
            fs::read_to_string(world.project_dir().join("tracked.txt")).unwrap_or_default()
                == "external\n",
            "stale edit must preserve external bytes",
        )?;

        let reobserve = send.send("read after external").expect("reobserve");
        let captures2 = server2.captured_requests();
        assert_clean_send(&reobserve)?;
        require(
            tool_ok(&reobserve.parsed, "observe.workspace"),
            "reobserve ok",
        )?;
        let base = captures2.len().saturating_sub(2);
        let seen = tool_result_value(&captures2[base + 1]).expect("reobserve result");
        let content = seen
            .pointer("/result/content")
            .and_then(Value::as_str)
            .or_else(|| seen.get("content").and_then(Value::as_str))
            .unwrap_or("");
        require(
            content.contains("external"),
            format!("reobserve must see external bytes: {seen}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("observe original", &observe.command)],
        captures_obs,
        result,
    );
}

/// Successful create followed by provider disconnect commits an error Turn
/// but retains the Local mutation (no implicit rollback).
#[test]
fn direct_mutation_no_rollback_after_provider_failure() {
    let driver = driver();
    let (world, spec) = mutation_world(
        "direct-mut-norollback",
        "dl-noroll",
        "[\"mutate.create\"]",
        "",
    );
    let server = wire_fake_llm(
        &world,
        "dl-noroll",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"kept.txt","content":"must-remain\n","create_parents":false}"#,
            ),
            SseScript::Disconnect {
                chunks_before_drop: vec!["partial".into()],
            },
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-noroll", "main").expect("session");
    let outcome = send.send("create then die").expect("turn drives");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(tool_ok(&outcome.parsed, "mutate.create"), "create ran")?;
        let path = world.project_dir().join("kept.txt");
        require(
            path.is_file() && fs::read_to_string(&path).unwrap_or_default() == "must-remain\n",
            "successful mutation must remain after provider failure",
        )?;
        let completed = outcome.has_committed_completed();
        let failed_or_error = is_non_completed_terminal(&outcome.parsed);
        require(
            !completed || failed_or_error,
            format!(
                "expected non-completed terminal after disconnect: {:?}",
                outcome.type_sequence()
            ),
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed)
                || outcome.exit_code == Some(SendExitCode::Committed),
            format!("unexpected exit: {:?}", outcome.exit_code),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("create then disconnect", &outcome.command)],
        captures,
        result,
    );
}

/// Write policy, system exclusion, and preflight effect denial produce no
/// filesystem side effect.
#[test]
fn direct_mutation_denials_have_no_side_effect() {
    let driver = driver();

    let (world, spec) = mutation_world(
        "direct-mut-deny-write",
        "dl-dwrite",
        "[\"mutate.create\"]",
        "\n[lancet.workspace]\ndenied_write_patterns = [\"blocked/**\"]\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-dwrite",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"blocked/x.txt","content":"nope","create_parents":true}"#,
            ),
            SseScript::plain("denied"),
        ]),
    );
    let mut scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-dwrite", "main").expect("session");
    let write_deny = send.send("write denied").expect("turn");
    let caps1 = server.captured_requests();
    drop(server);

    let (world2, spec2) =
        mutation_world("direct-mut-deny-sys", "dl-dsys", "[\"mutate.create\"]", "");
    let server2 = wire_fake_llm(
        &world2,
        "dl-dsys",
        sequence(vec![
            tool_call(
                "c2",
                "mutate.create",
                r#"{"path":".lattice/evil.txt","content":"nope","create_parents":true}"#,
            ),
            SseScript::plain("denied"),
        ]),
    );
    let mut evidence2 = evidence(&world2, &spec2);
    let send2 =
        SessionSendDriver::new(&driver, &world2, &spec2, "dl-dsys", "main").expect("session");
    let sys_deny = send2.send("write .lattice").expect("turn");
    let caps2 = server2.captured_requests();
    drop(server2);

    let (world3, spec3) = mutation_world(
        "direct-mut-deny-effect",
        "dl-deff",
        "[\"mutate.create\"]",
        "\n[lancet.effects]\ndeny = [\"workspace_mutation\"]\n",
    );
    let server3 = wire_fake_llm(
        &world3,
        "dl-deff",
        sequence(vec![
            tool_call(
                "c3",
                "mutate.create",
                r#"{"path":"effect-denied.txt","content":"nope","create_parents":false}"#,
            ),
            SseScript::plain("effect denied"),
        ]),
    );
    let evidence3 = evidence(&world3, &spec3);
    let send3 =
        SessionSendDriver::new(&driver, &world3, &spec3, "dl-deff", "main").expect("session");
    let effect_deny = send3.send("effect denied create").expect("turn");
    let caps3 = server3.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&write_deny)?;
        require(
            tool_failed(&write_deny.parsed, "mutate.create"),
            "write policy must fail tool",
        )?;
        require(
            !world.project_dir().join("blocked/x.txt").exists(),
            "write-policy denial left a file",
        )?;

        assert_clean_send(&sys_deny)?;
        require(
            tool_failed(&sys_deny.parsed, "mutate.create"),
            "system exclusion must fail tool",
        )?;
        require(
            !world2.project_dir().join(".lattice/evil.txt").exists(),
            "system-exclusion denial left a file",
        )?;

        assert_clean_send(&effect_deny)?;
        require(
            tool_failed(&effect_deny.parsed, "mutate.create"),
            "effect denial must fail tool",
        )?;
        require(
            !world3.project_dir().join("effect-denied.txt").exists(),
            "preflight effect denial left a file",
        )?;
        require(caps3.len() >= 2, "effect denial needs tool result capture")?;
        let err_text = last_tool_content(&caps3[1]).unwrap_or_default();
        require(
            err_text.to_lowercase().contains("effect")
                || err_text.to_lowercase().contains("denied")
                || err_text.to_lowercase().contains("policy"),
            format!("effect denial tool message weak: {err_text}"),
        )?;
        Ok(())
    })();

    scenario_evidence.record_command("write deny", &write_deny.command);
    scenario_evidence.record_captures(caps1);
    let _ = scenario_evidence.finish(false);
    evidence2.record_command("sys deny", &sys_deny.command);
    evidence2.record_captures(caps2);
    let _ = evidence2.finish(false);
    finish_scenario(
        evidence3,
        &[("effect deny", &effect_deny.command)],
        caps3,
        result,
    );
}

/// Two gated sends prepared from one session parent: one winner commits;
/// Local mutation assertions remain independent of session commit success.
#[test]
fn direct_mutation_concurrent_commit_race() {
    let driver = driver();
    let (world, spec) = mutation_world("direct-mut-race", "dl-race", "[\"mutate.create\"]", "");

    let (server, gate) = wire_gated_fake_llm(
        &world,
        "dl-race",
        sequence(vec![
            tool_call(
                "r1",
                "mutate.create",
                r#"{"path":"race-a.txt","content":"a\n","create_parents":false}"#,
            ),
            SseScript::plain("a done"),
            tool_call(
                "r2",
                "mutate.create",
                r#"{"path":"race-b.txt","content":"b\n","create_parents":false}"#,
            ),
            SseScript::plain("b done"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-race", "main").expect("session");

    let running_a = send.spawn("create a").expect("spawn a");
    let running_b = send.spawn("create b").expect("spawn b");

    let _ = server.wait_for_requests(1, Duration::from_secs(10));
    std::thread::sleep(Duration::from_millis(200));
    gate.release();

    let a = send.wait_spawned(running_a).expect("wait a");
    let b = send.wait_spawned(running_b).expect("wait b");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        let a_completed = a.has_committed_completed();
        let b_completed = b.has_committed_completed();
        require(
            a_completed || b_completed,
            format!(
                "expected at least one commit winner; a={:?} b={:?}",
                a.type_sequence(),
                b.type_sequence()
            ),
        )?;
        if a_completed && !b_completed {
            require(
                is_non_completed_terminal(&b.parsed),
                format!(
                    "loser b must fail deterministically: {:?}",
                    b.type_sequence()
                ),
            )?;
        }
        if b_completed && !a_completed {
            require(
                is_non_completed_terminal(&a.parsed),
                format!(
                    "loser a must fail deterministically: {:?}",
                    a.type_sequence()
                ),
            )?;
        }
        if tool_ok(&a.parsed, "mutate.create") || tool_ok(&b.parsed, "mutate.create") {
            require(
                world.project_dir().join("race-a.txt").is_file()
                    || world.project_dir().join("race-b.txt").is_file(),
                "successful mutate.create must leave Local bytes",
            )?;
        }
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("race a", &a.command), ("race b", &b.command)],
        captures,
        result,
    );
}

/// Lifecycle: tool activity precedes commit on a successful mutation turn.
#[test]
fn direct_mutation_tool_activity_before_commit() {
    let driver = driver();
    let (world, spec) = mutation_world("direct-mut-order", "dl-order", "[\"mutate.create\"]", "");
    let server = wire_fake_llm(
        &world,
        "dl-order",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"ordered.txt","content":"ok\n","create_parents":false}"#,
            ),
            SseScript::plain("done"),
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-order", "main").expect("session");
    let outcome = send.send("create ordered").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_event_subsequence(
            &outcome.parsed,
            &["tool_call_started", "tool_call_finished", "turn_committed"],
        )
        .map_err(|e| AssertError::StdoutMissing { expected: e })?;
        require(tool_ok(&outcome.parsed, "mutate.create"), "create ok")?;
        require(
            world.project_dir().join("ordered.txt").is_file(),
            "ordered.txt missing",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("ordered create", &outcome.command)],
        captures,
        result,
    );
}
