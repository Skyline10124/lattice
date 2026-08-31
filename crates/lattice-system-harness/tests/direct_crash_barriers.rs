// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Real subprocess/SIGKILL coverage for the production Turn barrier protocol.

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{AssertError, SessionSendDriver, SseScript};
use serde_json::Value;
use support::{
    assert_clean_send, assert_forced_kill, direct_world, driver, require, sequence, tool_call,
    wire_logged_fake_llm,
};

const BARRIER_ENV: &str = "LATTICE_TEST_TURN_BARRIER";
const ACK_ENV: &str = "LATTICE_TEST_TURN_BARRIER_ACK";
const INJECTION_ENV: &str = "LATTICE_TEST_TURN_INJECTION";

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
    assert_forced_kill(&killed);
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
fn next_model_attempt_sigkill_recovers_the_byte_exact_cumulative_transcript() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-crash-cumulative-transcript",
        "dl-crash-transcript",
        "[\"session.todo_write\"]",
        "",
    );
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let effect_log = outside.path().join("effects.log");
    world.set_env(
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        effect_log.display().to_string(),
    );
    let _server = wire_logged_fake_llm(
        &world,
        "dl-crash-transcript",
        sequence(vec![
            tool_call(
                "provider-private-call-id",
                "session.todo_write",
                r#"{"todos":[{"content":"persist me","status":"pending"}]}"#,
            ),
            SseScript::plain("new prompt after closure"),
        ]),
        &provider_log,
    );
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash-transcript", "main")
        .expect("session");
    let ack = outside.path().join("model-attempt-1.json");
    world.set_env(BARRIER_ENV, "model_attempt_started_1");
    world.set_env(ACK_ENV, ack.display().to_string());
    let running = send
        .spawn("kill after second attempt starts")
        .expect("spawn");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert_eq!(facts["barrier"], "model_attempt_started_1");
    assert_eq!(
        facts
            .pointer("/snapshot/stage/stage")
            .and_then(Value::as_str),
        Some("model_attempt_started")
    );
    let durable = facts["snapshot"]["post_user_transcript"]
        .as_array()
        .expect("cumulative transcript");
    assert_eq!(durable.len(), 2, "bound assistant + ToolResult: {facts}");
    assert_eq!(durable[0]["role"], "assistant");
    assert_eq!(durable[1]["role"], "tool");
    assert!(durable[0].to_string().contains("provider-private-call-id"));
    assert_eq!(fsynced_count(&provider_log), 1);
    assert_eq!(fsynced_count(&effect_log), 1);

    assert_forced_kill(&running.kill_and_wait().expect("kill"));
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);
    let recovery = send.send("close then run new prompt").expect("recovery");
    assert_clean_send(&recovery).expect("clean recovery");
    assert_eq!(fsynced_count(&provider_log), 2, "no provider replay");
    assert_eq!(fsynced_count(&effect_log), 1, "no Tool redispatch");
    assert!(!active_turn_exists(&world, send.session_id()));

    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log");
    let log_json: Value = serde_json::from_slice(&log.stdout).expect("log JSON");
    let recovered = log_json["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["origin"].as_str() == Some("reconciled"))
        .expect("reconciled Turn");
    let messages = committed_messages(&world, recovered["commit_id"].as_str().expect("commit id"));
    assert_eq!(messages.len(), 3, "user + exact durable transcript");
    assert_eq!(&messages[1..], durable.as_slice());
}

#[test]
fn post_mutation_step_closed_sigkill_reconciles_with_current_history_checkpoint() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-crash-mutation-step-closed",
        "dl-crash-step-checkpoint",
        "[\"mutate.create\"]",
        "",
    );
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let effect_log = outside.path().join("effects.log");
    world.set_env(
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        effect_log.display().to_string(),
    );
    let _server = wire_logged_fake_llm(
        &world,
        "dl-crash-step-checkpoint",
        sequence(vec![
            tool_call(
                "mutation-step-call",
                "mutate.create",
                r#"{"path":"reconciled-checkpoint.txt","content":"durable\n","create_parents":false}"#,
            ),
            SseScript::plain("fresh turn after reconciled closure"),
        ]),
        &provider_log,
    );
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash-step-checkpoint", "main")
        .expect("session");
    let ack = outside.path().join("mutation-step-closed.json");
    world.set_env(BARRIER_ENV, "step_closed");
    world.set_env(ACK_ENV, ack.display().to_string());
    let running = send
        .spawn("mutate then stop before TurnClosing")
        .expect("spawn");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert_eq!(facts["barrier"], "step_closed");
    assert_eq!(
        facts
            .pointer("/snapshot/stage/stage")
            .and_then(Value::as_str),
        Some("step_closed")
    );
    assert!(
        facts
            .pointer("/snapshot/stage/kind/continue_model")
            .is_some(),
        "the crash must precede TurnClosing at StepClosed(ContinueModel): {facts}"
    );
    let checkpoint = workspace_history_tip(&world);
    assert_eq!(checkpoint.len(), 40);
    assert_eq!(fsynced_count(&provider_log), 1);
    assert_eq!(fsynced_count(&effect_log), 1);
    assert_eq!(
        std::fs::read_to_string(world.project_dir().join("reconciled-checkpoint.txt")).unwrap(),
        "durable\n"
    );

    assert_forced_kill(&running.kill_and_wait().expect("kill"));
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);
    let recovery = send
        .send("fresh turn after closure")
        .expect("reconcile and run fresh Turn");
    assert_clean_send(&recovery).expect("clean recovery");
    assert_eq!(
        fsynced_count(&provider_log),
        2,
        "reconciliation must not call the provider; only the fresh Turn may do so"
    );
    assert_eq!(
        fsynced_count(&effect_log),
        1,
        "reconciliation must not redispatch the mutation"
    );
    assert!(!active_turn_exists(&world, send.session_id()));
    assert_eq!(
        std::fs::read_to_string(world.project_dir().join("reconciled-checkpoint.txt")).unwrap(),
        "durable\n"
    );

    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log");
    let log_json: Value = serde_json::from_slice(&log.stdout).expect("log JSON");
    let reconciled: Vec<_> = log_json["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .filter(|turn| turn["origin"].as_str() == Some("reconciled"))
        .collect();
    assert_eq!(
        reconciled.len(),
        1,
        "exactly one reconciled Turn: {log_json}"
    );
    assert!(reconciled[0]["outcome"].get("interrupted").is_some());
    assert_eq!(
        reconciled[0]
            .pointer("/workspace_history_checkpoint/oid")
            .and_then(Value::as_str),
        Some(checkpoint.as_str()),
        "the reconciler must sample and attach the current non-empty history OID"
    );
}

#[test]
fn post_mutation_turn_closing_sigkill_preserves_the_current_history_checkpoint() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-crash-mutation-checkpoint",
        "dl-crash-checkpoint",
        "[\"mutate.create\"]",
        "",
    );
    let outside = tempfile::tempdir().expect("external crash-surviving counters");
    let provider_log = outside.path().join("provider.log");
    let effect_log = outside.path().join("effects.log");
    world.set_env(
        "LATTICE_TEST_TOOL_EFFECT_LOG",
        effect_log.display().to_string(),
    );
    let _server = wire_logged_fake_llm(
        &world,
        "dl-crash-checkpoint",
        sequence(vec![
            tool_call(
                "mutation-call",
                "mutate.create",
                r#"{"path":"checkpoint.txt","content":"durable\n","create_parents":false}"#,
            ),
            SseScript::plain("mutation complete"),
        ]),
        &provider_log,
    );
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash-checkpoint", "main")
        .expect("session");
    let ack = outside.path().join("mutation-turn-closing.json");
    world.set_env(BARRIER_ENV, "turn_closing");
    world.set_env(ACK_ENV, ack.display().to_string());
    let running = send
        .spawn("mutate then stop before final refs")
        .expect("spawn");
    let facts = wait_for_ack(&ack, Duration::from_secs(20));
    assert_eq!(facts["barrier"], "turn_closing");
    assert_eq!(
        facts
            .pointer("/snapshot/stage/stage")
            .and_then(Value::as_str),
        Some("turn_closing"),
        "the crash must follow TurnClosing publication: {facts}"
    );
    let crashed_turn_id = facts
        .pointer("/snapshot/turn_id")
        .and_then(Value::as_str)
        .expect("crashed TurnId")
        .to_string();
    let checkpoint = facts
        .pointer("/snapshot/stage/candidate/workspace_history_checkpoint/oid")
        .and_then(Value::as_str)
        .expect("TurnClosing pins a non-empty history OID")
        .to_string();
    assert_eq!(checkpoint.len(), 40);
    assert_eq!(
        workspace_history_tip(&world),
        checkpoint,
        "TurnClosing must pin the exact current history OID"
    );
    assert_eq!(fsynced_count(&provider_log), 2);
    assert_eq!(fsynced_count(&effect_log), 1);
    assert_eq!(
        std::fs::read_to_string(world.project_dir().join("checkpoint.txt")).unwrap(),
        "durable\n"
    );

    assert_forced_kill(&running.kill_and_wait().expect("kill"));
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);
    let recovery = send.send("close then continue").expect("recovery");
    assert_clean_send(&recovery).expect("clean recovery");
    assert_eq!(fsynced_count(&provider_log), 3, "no provider replay");
    assert_eq!(fsynced_count(&effect_log), 1, "no mutation redispatch");
    assert!(!active_turn_exists(&world, send.session_id()));

    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log");
    let log_json: Value = serde_json::from_slice(&log.stdout).expect("log JSON");
    let recovered = log_json["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["turn_id"].as_str() == Some(crashed_turn_id.as_str()))
        .expect("exact crashed Turn");
    assert_eq!(
        recovered["origin"].as_str(),
        Some("live"),
        "post-TurnClosing recovery finalizes the pinned live candidate"
    );
    assert_eq!(
        recovered
            .pointer("/workspace_history_checkpoint/oid")
            .and_then(Value::as_str),
        Some(checkpoint.as_str())
    );
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
        BarrierCase::oversize("terminal_published", "step_closing"),
        BarrierCase::cancelled("cursor_1", "step_closing"),
        BarrierCase::oversize("step_closed", "step_closed"),
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum BarrierScenario {
    Plain,
    Todo,
    MultiTodo,
    Oversize,
    CancelledMutation,
}

#[derive(Clone, Copy)]
struct BarrierCase {
    barrier: &'static str,
    stage: &'static str,
    invocation_count: usize,
    lifecycle: Option<&'static str>,
    draft_classification: Option<&'static str>,
    scenario: BarrierScenario,
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
            scenario: BarrierScenario::Plain,
        }
    }

    const fn tool(
        barrier: &'static str,
        stage: &'static str,
        invocation_count: usize,
        lifecycle: Option<&'static str>,
    ) -> Self {
        Self {
            scenario: BarrierScenario::Todo,
            ..Self::plain(barrier, stage, invocation_count, lifecycle)
        }
    }

    const fn multi_tool(barrier: &'static str) -> Self {
        Self {
            scenario: BarrierScenario::MultiTodo,
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

    const fn oversize(barrier: &'static str, stage: &'static str) -> Self {
        Self {
            scenario: BarrierScenario::Oversize,
            ..Self::plain(barrier, stage, 1, Some("terminal"))
        }
    }

    const fn cancelled(barrier: &'static str, stage: &'static str) -> Self {
        Self {
            scenario: BarrierScenario::CancelledMutation,
            ..Self::plain(barrier, stage, 1, Some("terminal"))
        }
    }

    fn tools(self) -> &'static str {
        match self.scenario {
            BarrierScenario::Plain => "[]",
            BarrierScenario::Todo | BarrierScenario::MultiTodo | BarrierScenario::Oversize => {
                "[\"session.todo_write\"]"
            }
            BarrierScenario::CancelledMutation => "[\"mutate.create\"]",
        }
    }

    fn tool_count(self) -> usize {
        match self.scenario {
            BarrierScenario::Plain => 0,
            BarrierScenario::MultiTodo => 2,
            _ => 1,
        }
    }

    fn injection(self) -> Option<&'static str> {
        match self.scenario {
            BarrierScenario::CancelledMutation => Some("cancel_before_dispatch"),
            _ => None,
        }
    }

    fn expected_reconciled_outcome(self) -> &'static str {
        match self.scenario {
            BarrierScenario::CancelledMutation => "cancelled",
            _ => "interrupted",
        }
    }

    fn expected_effects(self) -> usize {
        match self.scenario {
            BarrierScenario::Plain | BarrierScenario::CancelledMutation => 0,
            BarrierScenario::MultiTodo if self.barrier == "cursor_1" => 1,
            BarrierScenario::MultiTodo => 2,
            BarrierScenario::Todo | BarrierScenario::Oversize => usize::from(matches!(
                self.barrier,
                "candidate_computed"
                    | "candidate_pinned"
                    | "draft_published"
                    | "terminal_published"
                    | "cursor_1"
                    | "step_closed"
                    | "turn_closing"
            )),
        }
    }
}

fn run_barrier_case(case: BarrierCase) {
    let driver = driver();
    let tools = case.tools();
    let (world, spec) = direct_world(
        &format!("direct-crash-{}", case.barrier.replace('_', "-")),
        "dl-crash-matrix",
        tools,
        "",
    );
    let mut scripts = vec![SseScript::plain("baseline complete")];
    match case.scenario {
        BarrierScenario::Plain => {}
        BarrierScenario::Todo => scripts.push(tool_call(
            "provider-private-call-id",
            "session.todo_write",
            r#"{"todos":[{"content":"crash matrix","status":"pending"}]}"#,
        )),
        BarrierScenario::MultiTodo => scripts.push(SseScript::ToolCalls {
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
        BarrierScenario::Oversize => {
            let args = serde_json::json!({
                "todos": [{"content": "x".repeat(70_000), "status": "pending"}]
            })
            .to_string();
            scripts.push(tool_call(
                "provider-private-oversize-call",
                "session.todo_write",
                &args,
            ));
        }
        BarrierScenario::CancelledMutation => scripts.push(tool_call(
            "provider-private-cancelled-call",
            "mutate.create",
            r#"{"path":"must-not-exist.txt","content":"forbidden\n","create_parents":false}"#,
        )),
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
    if let Some(injection) = case.injection() {
        world.set_env(INJECTION_ENV, injection);
    }
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
    if case.tool_count() == 2 {
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
    match case.scenario {
        BarrierScenario::Oversize => {
            let record = invocations.values().next().expect("oversize invocation");
            assert_eq!(
                record
                    .pointer("/state/terminal/material/unavailable/reason")
                    .and_then(Value::as_str),
                Some("output_too_large"),
                "{facts}"
            );
            assert!(
                record
                    .pointer("/state/terminal/material/unavailable/digest")
                    .and_then(Value::as_str)
                    .is_some_and(|digest| digest.len() == 64),
                "{facts}"
            );
            assert!(
                record
                    .pointer("/state/terminal/material/unavailable/byte_length")
                    .and_then(Value::as_u64)
                    .is_some_and(|length| length > 65_536),
                "{facts}"
            );
            if case.barrier == "terminal_published" {
                assert_eq!(
                    session_facts
                        .pointer("/snapshot/stage/current_candidate")
                        .unwrap_or(&Value::Null),
                    &Value::Null,
                    "oversize result must not pin a ResultDraft: {facts}"
                );
            }
        }
        BarrierScenario::CancelledMutation => {
            let record = invocations.values().next().expect("cancelled invocation");
            assert_eq!(
                record
                    .pointer("/state/terminal/outcome/not_executed/reason")
                    .and_then(Value::as_str),
                Some("cancelled_before_dispatch"),
                "{facts}"
            );
            assert_eq!(
                session_facts
                    .pointer("/snapshot/stage/closures/0/outcome")
                    .and_then(Value::as_str),
                Some("not_executed"),
                "conversation closure must stay lossy: {facts}"
            );
            assert!(!world.project_dir().join("must-not-exist.txt").exists());
            assert!(
                !workspace_history_has_current(&world),
                "cancelled call moved WorkspaceHistory"
            );
        }
        _ => {}
    }
    let expected_pre_kill = if case.barrier == "turn_closing" {
        3
    } else if case.tool_count() > 0 {
        2
    } else {
        1
    };
    let expected_effects = case.expected_effects();
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
    assert_forced_kill(&running.kill_and_wait().expect("kill send"));
    world.unset_env(BARRIER_ENV);
    world.unset_env(ACK_ENV);
    world.unset_env(INJECTION_ENV);
    let recovery = send
        .send("recover killed turn")
        .expect("reopen and reconcile");
    assert_clean_send(&recovery).expect("recovery command clean");
    assert!(
        recovery.command.is_clean_exit(0),
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
    if case.scenario == BarrierScenario::CancelledMutation {
        assert!(!world.project_dir().join("must-not-exist.txt").exists());
        assert!(
            !workspace_history_has_current(&world),
            "reopen moved WorkspaceHistory for cancelled call"
        );
    }
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
        let expected = case.expected_reconciled_outcome();
        assert!(
            recovered_turns.iter().any(|turn| {
                turn["origin"].as_str() == Some("reconciled")
                    && turn["outcome"].get(expected).is_some()
            }),
            "expected reconciled {expected} outcome: {recovery_json}"
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

fn workspace_history_has_current(world: &lattice_system_harness::ScenarioWorld) -> bool {
    let projects = world.home().join(".local/state/lattice/projects");
    let repos: Vec<_> = std::fs::read_dir(projects)
        .expect("project stores")
        .map(|entry| {
            entry
                .expect("project store entry")
                .path()
                .join("workspace-history.git")
        })
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(repos.len(), 1, "one project workspace-history store");
    let repo =
        gix::open_opts(&repos[0], gix::open::Options::isolated()).expect("open workspace history");
    repo.try_find_reference("refs/lattice/workspace-history/current")
        .expect("read current workspace-history ref")
        .is_some()
}

fn workspace_history_tip(world: &lattice_system_harness::ScenarioWorld) -> String {
    let projects = world.home().join(".local/state/lattice/projects");
    let repos: Vec<_> = std::fs::read_dir(projects)
        .expect("project stores")
        .map(|entry| {
            entry
                .expect("project store entry")
                .path()
                .join("workspace-history.git")
        })
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(repos.len(), 1, "one project workspace-history store");
    let repo =
        gix::open_opts(&repos[0], gix::open::Options::isolated()).expect("open workspace history");
    repo.find_reference("refs/lattice/workspace-history/current")
        .expect("current workspace-history ref")
        .target()
        .try_id()
        .expect("direct current workspace-history ref")
        .to_string()
}

fn committed_messages(
    world: &lattice_system_harness::ScenarioWorld,
    commit_id: &str,
) -> Vec<Value> {
    let repo = gix::open_opts(
        world.session_store_dir().join("sessions.git"),
        gix::open::Options::isolated(),
    )
    .expect("open Session store");
    let oid = gix::ObjectId::from_hex(commit_id.as_bytes()).expect("parse commit id");
    let tree = repo
        .find_commit(oid)
        .expect("find Turn commit")
        .tree()
        .expect("Turn tree");
    let mut files = Vec::new();
    collect_tree_files(&tree, "", &mut files);
    let bytes = files
        .into_iter()
        .find_map(|(path, bytes)| (path == "turn/messages.jsonl").then_some(bytes))
        .expect("turn/messages.jsonl");
    String::from_utf8(bytes)
        .expect("messages UTF-8")
        .lines()
        .map(|line| serde_json::from_str(line).expect("typed message JSON"))
        .collect()
}

fn collect_tree_files(tree: &gix::Tree<'_>, prefix: &str, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in tree.iter() {
        let entry = entry.expect("tree entry");
        let name = entry.filename().to_string();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let object = entry.object().expect("tree object");
        match object.kind {
            gix::object::Kind::Tree => {
                collect_tree_files(&object.try_into_tree().expect("subtree"), &path, out)
            }
            gix::object::Kind::Blob => {
                out.push((path, object.try_into_blob().expect("blob").data.clone()))
            }
            _ => {}
        }
    }
}

fn wait_for_ack(path: &std::path::Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(facts) = serde_json::from_slice(&bytes)
        {
            return facts;
        }
        assert!(Instant::now() < deadline, "barrier ack was not published");
        std::thread::sleep(Duration::from_millis(5));
    }
}
