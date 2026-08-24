// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Real subprocess/SIGKILL coverage for the production Turn barrier protocol.

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{AssertError, SessionSendDriver, SseScript, Termination};
use serde_json::Value;
use support::{
    assert_clean_send, direct_world, driver, require, sequence, tool_call, wire_logged_fake_llm,
};

const BARRIER_ENV: &str = "LATTICE_TEST_TURN_BARRIER";
const ACK_ENV: &str = "LATTICE_TEST_TURN_BARRIER_ACK";

#[test]
fn turn_started_sigkill_reopens_without_replaying_the_provider() {
    let driver = driver();
    let (world, spec) = direct_world("direct-crash-turn-started", "dl-crash", "[]", "");
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let server = wire_logged_fake_llm(
        &world,
        "dl-crash",
        sequence(vec![
            SseScript::plain("baseline complete"),
            SseScript::plain("post-recovery complete"),
        ]),
        &provider_log,
    );
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash", "main").expect("session");

    let baseline = send.send("baseline").expect("baseline turn");
    assert_clean_send(&baseline).expect("baseline clean");
    require(
        server.captured_requests().len() == 1,
        "baseline must establish a non-zero provider count",
    )
    .unwrap();
    assert_eq!(fsynced_count(&provider_log), 1);

    let ack = outside.path().join("turn-started.json");
    world.set_env(BARRIER_ENV, "turn_started");
    world.set_env(ACK_ENV, ack.display().to_string());
    let running = send
        .spawn("kill after durable TurnStarted")
        .expect("spawn send");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert!(active_turn_exists(&world, send.session_id()));

    let result: Result<(), AssertError> = (|| {
        require(
            facts.get("barrier").and_then(Value::as_str) == Some("turn_started"),
            format!("wrong barrier facts: {facts}"),
        )?;
        require(
            facts
                .pointer("/snapshot/stage/stage")
                .and_then(Value::as_str)
                == Some("turn_started"),
            format!("ack did not observe durable TurnStarted: {facts}"),
        )?;
        require(
            facts
                .get("active_turn_oid")
                .and_then(Value::as_str)
                .is_some_and(|oid| oid.len() == 40),
            format!("ack omitted active-turn oid: {facts}"),
        )?;
        require(
            facts
                .get("invocations")
                .and_then(Value::as_object)
                .is_some_and(serde_json::Map::is_empty),
            format!("TurnStarted must precede every invocation reservation: {facts}"),
        )?;
        require(
            server.captured_requests().len() == 1,
            "provider count changed before the TurnStarted barrier",
        )?;
        Ok(())
    })();
    result.unwrap_or_else(|error| panic!("{error}"));

    let killed = running.kill_and_wait().expect("SIGKILL send process group");
    assert_eq!(killed.termination, Termination::Killed);
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);

    let resumed = send
        .send("new turn after crash")
        .expect("reopen and continue");
    assert_clean_send(&resumed).expect("recovered send clean");
    assert_eq!(
        server.captured_requests().len(),
        2,
        "reopen must close the killed Turn without replaying its provider request",
    );
    assert_eq!(fsynced_count(&provider_log), 2);
    assert!(!active_turn_exists(&world, send.session_id()));

    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log");
    let log_json: Value = serde_json::from_slice(&log.stdout).expect("session log JSON");
    let turns = log_json["turns"].as_array().expect("turn array");
    assert_eq!(
        turns.len(),
        3,
        "baseline + reconciled + new Turn: {log_json}"
    );
    assert!(turns.iter().any(|turn| {
        turn.get("origin").and_then(Value::as_str) == Some("reconciled")
            && turn
                .get("outcome")
                .and_then(Value::as_object)
                .is_some_and(|outcome| outcome.contains_key("interrupted"))
    }));
}

#[test]
fn model_and_binding_windows_recover_without_provider_replay() {
    for case in [
        BarrierCase::plain("model_attempt_started", "model_attempt_started", 0, None),
        BarrierCase::tool("binding_planned", "binding_planned", 0, None),
        BarrierCase::tool(
            "reservation_published",
            "binding_planned",
            1,
            Some("reserved"),
        ),
        BarrierCase::tool("bound", "bound", 1, Some("reserved")),
        BarrierCase::tool("step_closing_empty", "step_closing", 1, Some("reserved")),
        BarrierCase::tool(
            "dispatch_started",
            "step_closing",
            1,
            Some("dispatch_started"),
        ),
    ] {
        run_barrier_case(case);
    }
}

#[test]
fn result_terminal_and_cursor_windows_recover_without_redispatch() {
    for case in [
        BarrierCase::tool(
            "candidate_computed",
            "step_closing",
            1,
            Some("dispatch_started"),
        ),
        BarrierCase::tool_with_draft(
            "candidate_pinned",
            "step_closing",
            "clean_absent",
            Some("dispatch_started"),
        ),
        BarrierCase::tool_with_draft(
            "draft_published",
            "step_closing",
            "durable",
            Some("dispatch_started"),
        ),
        BarrierCase::tool_with_draft(
            "terminal_published",
            "step_closing",
            "durable",
            Some("terminal"),
        ),
        BarrierCase::tool("cursor_1", "step_closing", 1, Some("terminal")),
        BarrierCase::tool("step_closed", "step_closed", 1, Some("terminal")),
        BarrierCase::tool("turn_closing", "turn_closing", 1, Some("terminal")),
    ] {
        run_barrier_case(case);
    }
}

#[test]
fn every_multi_tool_cursor_recovers_without_redispatch() {
    for case in [
        BarrierCase::multi_tool("cursor_1"),
        BarrierCase::multi_tool("cursor_2"),
    ] {
        run_barrier_case(case);
    }
}

#[derive(Clone, Copy)]
struct BarrierCase {
    barrier: &'static str,
    stage: &'static str,
    invocation_count: usize,
    lifecycle: Option<&'static str>,
    draft_classification: Option<&'static str>,
    tool_bearing: bool,
    tool_count: usize,
}

impl BarrierCase {
    const fn plain(
        barrier: &'static str,
        stage: &'static str,
        invocation_count: usize,
        lifecycle: Option<&'static str>,
    ) -> Self {
        Self {
            barrier,
            stage,
            invocation_count,
            lifecycle,
            draft_classification: None,
            tool_bearing: false,
            tool_count: 0,
        }
    }

    const fn tool(
        barrier: &'static str,
        stage: &'static str,
        invocation_count: usize,
        lifecycle: Option<&'static str>,
    ) -> Self {
        Self {
            tool_bearing: true,
            tool_count: 1,
            ..Self::plain(barrier, stage, invocation_count, lifecycle)
        }
    }

    const fn multi_tool(barrier: &'static str) -> Self {
        Self {
            tool_bearing: true,
            tool_count: 2,
            ..Self::plain(barrier, "step_closing", 2, None)
        }
    }
    const fn tool_with_draft(
        barrier: &'static str,
        stage: &'static str,
        draft_classification: &'static str,
        lifecycle: Option<&'static str>,
    ) -> Self {
        Self {
            draft_classification: Some(draft_classification),
            ..Self::tool(barrier, stage, 1, lifecycle)
        }
    }
}

fn run_barrier_case(case: BarrierCase) {
    let driver = driver();
    let tools = if case.tool_bearing {
        "[\"session.todo_write\"]"
    } else {
        "[]"
    };
    let (world, spec) = direct_world(
        &format!("direct-crash-{}", case.barrier.replace('_', "-")),
        "dl-crash-matrix",
        tools,
        "",
    );
    let mut scripts = vec![SseScript::plain("baseline complete")];
    match case.tool_count {
        0 => {}
        1 => scripts.push(tool_call(
            "provider-private-call-id",
            "session.todo_write",
            r#"{"todos":[{"content":"crash matrix","status":"pending"}]}"#,
        )),
        2 => scripts.push(SseScript::ToolCalls {
            calls: vec![
                (
                    "provider-private-call-1".into(),
                    "session.todo_write".into(),
                    r#"{"todos":[{"content":"first","status":"pending"}]}"#.into(),
                ),
                (
                    "provider-private-call-2".into(),
                    "session.todo_write".into(),
                    r#"{"todos":[{"content":"second","status":"pending"}]}"#.into(),
                ),
            ],
        }),
        _ => unreachable!(),
    }
    if case.barrier == "turn_closing" {
        scripts.push(SseScript::plain(
            "tool turn completed before final transaction",
        ));
    }
    scripts.push(SseScript::plain("post-recovery complete"));
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let effect_log = outside.path().join("effects.log");
    world.set_env(
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        effect_log.display().to_string(),
    );
    let server = wire_logged_fake_llm(&world, "dl-crash-matrix", sequence(scripts), &provider_log);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-crash-matrix", "main").expect("session");
    assert_clean_send(&send.send("baseline").expect("baseline")).expect("baseline clean");

    let ack = outside.path().join(format!("{}.json", case.barrier));
    world.set_env(BARRIER_ENV, case.barrier);
    world.set_env(ACK_ENV, ack.display().to_string());
    let running = send.spawn("kill at durable barrier").expect("spawn send");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert_eq!(facts["barrier"], case.barrier);
    assert!(active_turn_exists(&world, send.session_id()));
    let session_facts = if case.barrier == "dispatch_started" {
        &facts["session_context"]
    } else {
        &facts
    };
    assert_eq!(
        session_facts.pointer("/snapshot/stage/stage"),
        Some(&Value::from(case.stage))
    );
    let invocations = session_facts["invocations"]
        .as_object()
        .expect("invocation map");
    assert_eq!(invocations.len(), case.invocation_count, "{facts}");
    if let Some(expected) = case.lifecycle {
        let state = if case.barrier == "dispatch_started" {
            &facts["record"]["state"]
        } else {
            &invocations.values().next().expect("invocation")["state"]
        };
        let actual = if state.is_string() {
            state.as_str()
        } else if state.get("terminal").is_some() {
            Some("terminal")
        } else {
            None
        };
        assert_eq!(actual, Some(expected), "{facts}");
    }
    if case.tool_count == 2 {
        let terminal = invocations
            .values()
            .filter(|record| record["state"].get("terminal").is_some())
            .count();
        let reserved = invocations
            .values()
            .filter(|record| record["state"].as_str() == Some("reserved"))
            .count();
        if case.barrier == "cursor_1" {
            assert_eq!((terminal, reserved), (1, 1), "{facts}");
        } else {
            assert_eq!((terminal, reserved), (2, 0), "{facts}");
        }
    }
    if let Some(expected) = case.draft_classification {
        assert_eq!(
            session_facts["draft_classification"].as_str(),
            Some(expected),
            "{facts}"
        );
    }
    let expected_pre_kill = if case.barrier == "turn_closing" {
        3
    } else if case.tool_bearing {
        2
    } else {
        1
    };
    let expected_effects = match case.barrier {
        "cursor_1" => 1,
        "cursor_2" => 2,
        "candidate_computed" | "candidate_pinned" | "draft_published" | "terminal_published"
        | "step_closed" | "turn_closing" => 1,
        _ => 0,
    };
    assert_eq!(server.captured_requests().len(), expected_pre_kill);
    assert_eq!(fsynced_count(&provider_log), expected_pre_kill);
    assert_eq!(fsynced_count(&effect_log), expected_effects);
    if expected_effects > 0 {
        let effect_ids = std::fs::read_to_string(&effect_log).expect("effect log");
        assert!(
            effect_ids
                .lines()
                .all(|invocation_id| invocations.contains_key(invocation_id)),
            "effect log InvocationIds differ from the durable ledger: {facts}"
        );
    }
    assert_eq!(
        running.kill_and_wait().expect("kill send").termination,
        Termination::Killed
    );
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);
    let recovery = send
        .send("recover killed turn")
        .expect("reopen and reconcile");
    assert_clean_send(&recovery).expect("recovery command clean");
    assert!(
        recovery.command.status.success(),
        "recovery command failed at {}: stdout={} stderr={}",
        case.barrier,
        String::from_utf8_lossy(&recovery.command.stdout),
        String::from_utf8_lossy(&recovery.command.stderr),
    );
    assert_eq!(
        server.captured_requests().len(),
        expected_pre_kill + 1,
        "recovery must close the killed Turn without replay, then execute one new request for {}",
        case.barrier
    );
    assert_eq!(fsynced_count(&provider_log), expected_pre_kill + 1);
    assert!(!active_turn_exists(&world, send.session_id()));
    assert_eq!(
        fsynced_count(&effect_log),
        expected_effects,
        "reopen must not redispatch a Tool at {}",
        case.barrier
    );
    let recovery_log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("recovery log");
    let recovery_json: Value =
        serde_json::from_slice(&recovery_log.stdout).expect("recovery log JSON");
    let recovered_turns = recovery_json["turns"].as_array().expect("recovery turns");
    assert_eq!(recovered_turns.len(), 3, "{recovery_json}");
    if case.barrier == "turn_closing" {
        assert!(
            recovered_turns
                .iter()
                .any(|turn| turn["origin"].as_str() == Some("live")
                    && turn["outcome"].get("completed").is_some()),
            "{recovery_json}"
        );
    } else {
        assert!(
            recovered_turns
                .iter()
                .any(|turn| turn["origin"].as_str() == Some("reconciled")
                    && turn["outcome"].get("interrupted").is_some()),
            "{recovery_json}"
        );
    }
    assert!(
        recovered_turns
            .last()
            .is_some_and(|turn| turn["origin"].as_str() == Some("live")
                && turn["outcome"].get("completed").is_some()),
        "{recovery_json}"
    );
}
fn active_turn_exists(world: &lattice_system_harness::ScenarioWorld, session_id: &str) -> bool {
    let repo = gix::open_opts(
        world.session_store_dir().join("sessions.git"),
        gix::open::Options::isolated(),
    )
    .expect("open Session store");
    repo.try_find_reference(&format!("refs/lattice/sessions/{session_id}/active-turn"))
        .expect("read active-turn ref")
        .is_some()
}

fn fsynced_count(path: &std::path::Path) -> usize {
    std::fs::read_to_string(path)
        .map(|content| content.lines().count())
        .unwrap_or(0)
}

fn wait_for_ack(path: &std::path::Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        if path.is_file() {
            let bytes = std::fs::read(path).expect("read barrier ack");
            return serde_json::from_slice(&bytes).expect("barrier ack JSON");
        }
        assert!(Instant::now() < deadline, "barrier ack was not published");
        std::thread::sleep(Duration::from_millis(5));
    }
}
