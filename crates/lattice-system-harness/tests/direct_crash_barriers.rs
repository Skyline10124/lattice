// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 用真实后台进程的已确认SIGSTOP/SIGKILL窗口验证Turn关闭，不让前端死亡冒充后台崩溃。

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{SessionSendDriver, SseScript};
use serde_json::{Value, json};
use support::{
    assert_clean_send, direct_world, driver, sequence, tool_call, wire_gated_fake_llm,
    wire_logged_fake_llm,
};
const BARRIER_ENV: &str = "LATTICE_TEST_TURN_BARRIER";
const ACK_ENV: &str = "LATTICE_TEST_TURN_BARRIER_ACK";
const INJECTION_ENV: &str = "LATTICE_TEST_TURN_INJECTION";

#[test]
fn crash_start_cannot_reset_an_expired_scenario_budget() {
    let (world, spec) = direct_world("crash-budget-expired", "dl-budget", "[]", "");
    let budget = lattice_system_harness::ScenarioBudget::new(Some(Duration::ZERO));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let daemon = support::crash::start(&world, &spec, &budget);
        let stopped = world.stop_daemon();
        drop(daemon);
        stopped.unwrap();
    }));
    let error = result.expect_err("expired scenario must reject auxiliary startup");
    let message = error.downcast_ref::<String>().unwrap();
    assert!(message.contains("exceeded total timeout"), "{message}");
    assert!(world.daemon_pids().unwrap().is_empty());
}

#[test]
fn turn_started_sigkill_reopens_without_replaying_the_provider() {
    let driver = driver();
    let (world, spec) = direct_world("direct-crash-turn-started", "dl-crash", "[]", "");
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let outside = tempfile::tempdir().unwrap();
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
        let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash", "main").unwrap();
        let baseline = send.send("baseline").unwrap();
        evidence.record_command("baseline", &baseline.command);
        assert_clean_send(&baseline).unwrap();
        assert!(baseline.command.is_clean_exit(0));
        assert_eq!(fsynced_count(&provider_log), 1);
        let ack = outside.path().join("turn-started.json");
        world.set_env(BARRIER_ENV, "turn_started");
        world.set_env(ACK_ENV, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send.spawn("kill after durable TurnStarted").unwrap();
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
        assert_eq!(facts["barrier"], "turn_started");
        assert_eq!(facts["snapshot"]["stage"]["stage"], "turn_started");
        assert_eq!(facts["active_turn_oid"].as_str().unwrap().len(), 40);
        assert!(facts["invocations"].as_object().unwrap().is_empty());
        assert_eq!(server.captured_requests().len(), 1);
        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        assert!(active_turn_exists(&send));
        assert_eq!(
            fsynced_count(&provider_log),
            1,
            "read-only reconnect must not recover or execute"
        );
        let recovered = send
            .send("new prompt must not be accepted during recovery")
            .unwrap();
        evidence.record_command("prompt only closes the old Turn", &recovered.command);
        assert_clean_send(&recovered).unwrap();
        assert!(recovered.command.is_clean_exit(1));
        assert!(recovered.accepted_turn_id().is_none());
        assert_eq!(recovered.committed_result().unwrap().2, "interrupted");
        assert_recovery_confirmation(&send, facts["turn_id"].as_str().unwrap(), &mut evidence);
        assert_eq!(server.captured_requests().len(), 1);
        assert_eq!(fsynced_count(&provider_log), 1);
        assert!(!active_turn_exists(&send));
        let new_turn = send.send("explicit new prompt after recovery").unwrap();
        evidence.record_command("explicit new Turn", &new_turn.command);
        assert_clean_send(&new_turn).unwrap();
        assert!(new_turn.command.is_clean_exit(0));
        assert_eq!(fsynced_count(&provider_log), 2);
        assert_eq!(server.captured_requests().len(), 2);
        let log = send
            .run_command(&["--json", "session", "log", send.session_id()])
            .unwrap();
        evidence.record_command("log after explicit new Turn", &log);
        assert!(log.is_clean_exit(0));
        let value: Value = serde_json::from_slice(&log.stdout).unwrap();
        let turns = value["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert!(turns.iter().any(|turn| turn["turn_id"] == facts["turn_id"]
            && turn["origin"] == "reconciled"
            && turn["outcome"].get("interrupted").is_some()));
        evidence.record_captures(server.captured_requests());
    }));
    support::crash::finish(evidence, result);
}

fn assert_recovery_confirmation(
    send: &SessionSendDriver<'_>,
    turn_id: &str,
    evidence: &mut lattice_system_harness::ScenarioEvidence,
) {
    let observed = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            send.session_id(),
            turn_id,
            "--follow",
        ])
        .unwrap();
    evidence.record_command("follow the recovered old Turn confirmation", &observed);
    assert!(
        observed.is_clean_exit(1),
        "interrupted outcome: {}",
        observed.summary()
    );
    let records: Vec<Value> = std::str::from_utf8(&observed.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        !records.iter().any(|record| record["type"] == "error"),
        "{records:?}"
    );
    assert!(records.iter().any(|record| {
        record
            .pointer("/view/requested_turn/turn_id")
            .and_then(Value::as_str)
            == Some(turn_id)
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
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send
            .spawn("kill after second attempt starts")
            .expect("spawn");
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
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

        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        assert!(active_turn_exists(&send));
        assert_eq!(fsynced_count(&provider_log), 1, "query must not recover");
        let recovery = send
            .run_command(&["--json", "session", "recover", send.session_id()])
            .unwrap();
        evidence.record_command("independent closure-only recovery", &recovery);
        assert!(recovery.is_clean_exit(0));
        assert_recovery_confirmation(&send, facts["turn_id"].as_str().unwrap(), &mut evidence);
        assert_eq!(
            fsynced_count(&provider_log),
            1,
            "no provider replay or new prompt"
        );
        assert_eq!(fsynced_count(&effect_log), 1, "no Tool redispatch");
        assert!(!active_turn_exists(&send));

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
        let messages =
            committed_messages(&send, recovered["commit_id"].as_str().expect("commit id"));
        assert_eq!(messages.len(), 3, "user + exact durable transcript");
        assert_eq!(&messages[1..], durable.as_slice());
        evidence.record_command("recovery log", &log);
    }));
    support::crash::finish(evidence, result);
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
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let send =
            SessionSendDriver::new(&driver, &world, &spec, "dl-crash-step-checkpoint", "main")
                .expect("session");
        let ack = outside.path().join("mutation-step-closed.json");
        world.set_env(BARRIER_ENV, "step_closed");
        world.set_env(ACK_ENV, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send
            .spawn("mutate then stop before TurnClosing")
            .expect("spawn");
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
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

        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        assert!(active_turn_exists(&send));
        let recovery = send
            .run_command(&["--json", "session", "recover", send.session_id()])
            .unwrap();
        evidence.record_command("recover mutation StepClosed", &recovery);
        assert!(recovery.is_clean_exit(0));
        assert_eq!(
            fsynced_count(&provider_log),
            1,
            "closure-only must not call the provider"
        );
        assert_eq!(
            fsynced_count(&effect_log),
            1,
            "reconciliation must not redispatch the mutation"
        );
        assert!(!active_turn_exists(&send));
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
        evidence.record_command("checkpoint recovery log", &log);
    }));
    support::crash::finish(evidence, result);
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
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send
            .spawn("mutate then stop before final refs")
            .expect("spawn");
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
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

        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        assert!(active_turn_exists(&send));
        let recovery = send
            .run_command(&["--json", "session", "recover", send.session_id()])
            .unwrap();
        evidence.record_command("finalize exact TurnClosing candidate", &recovery);
        assert!(recovery.is_clean_exit(0));
        assert_eq!(
            fsynced_count(&provider_log),
            2,
            "no provider replay or new Turn"
        );
        assert_eq!(fsynced_count(&effect_log), 1, "no mutation redispatch");
        assert!(!active_turn_exists(&send));

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
        evidence.record_command("exact candidate recovery log", &log);
    }));
    support::crash::finish(evidence, result);
}

/// 已持久 ModelAttemptInterrupted(Cancelled) 但 StepClosed 前真实终止后台：
/// 显式 recover 得到顶层 Cancelled，历史与已发生效果保持，且不重放 provider/Tool。
#[test]
fn interrupted_cancelled_attempt_before_step_closed_recovers_as_cancelled() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-crash-interrupted-cancelled",
        "dl-crash-cancelled",
        "[\"mutate.create\"]",
        "",
    );
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let outside = tempfile::tempdir().expect("external crash-surviving counters");
        let effect_log = outside.path().join("effects.log");
        world.set_env(
            "LATTICE_TEST_TOOL_EFFECT_LOG",
            effect_log.display().to_string(),
        );
        let (server, gate) = wire_gated_fake_llm(
            &world,
            "dl-crash-cancelled",
            sequence(vec![
                tool_call(
                    "provider-private-cancelled-window-call",
                    "mutate.create",
                    r#"{"path":"cancelled-window.txt","content":"kept\n","create_parents":false}"#,
                ),
                SseScript::plain("never delivered after the cancel"),
            ]),
        );
        let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash-cancelled", "main")
            .expect("session");
        let ack = outside.path().join("model-attempt-interrupted.json");
        world.set_env(BARRIER_ENV, "model_attempt_interrupted");
        world.set_env(ACK_ENV, ack.display().to_string());
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send
            .spawn("cancel during the second model attempt")
            .expect("spawn");
        let stall = send.budget().cap_timeout(Duration::from_secs(30));
        // 第一个模型请求交回 Tool 调用：真实效果先落地，随后才取消在途尝试。
        gate.wait_until_stalled(1, stall).unwrap();
        gate.release_next(stall).unwrap();
        gate.wait_until_stalled(2, stall).unwrap();
        let turn_id = active_turn(&send).expect("accepted Turn is active");
        let cancel = send
            .run_command(&[
                "--json",
                "session",
                "cancel",
                send.session_id(),
                turn_id.as_str(),
            ])
            .unwrap();
        evidence.record_command("cancel the in-flight model attempt", &cancel);
        assert!(cancel.is_clean_exit(0), "{}", cancel.summary());
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
        assert_eq!(facts["barrier"], "model_attempt_interrupted");
        assert_eq!(facts["turn_id"], json!(turn_id));
        assert_eq!(
            facts
                .pointer("/snapshot/stage/stage")
                .and_then(Value::as_str),
            Some("model_attempt_interrupted"),
            "the crash must follow the durable interruption: {facts}"
        );
        assert_eq!(
            facts
                .pointer("/snapshot/stage/reason")
                .and_then(Value::as_str),
            Some("cancelled"),
            "the durable interruption reason must be the caller cancel: {facts}"
        );
        assert_eq!(fsynced_count(&effect_log), 1);
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("cancelled-window.txt")).unwrap(),
            "kept\n"
        );
        assert_eq!(server.captured_requests().len(), 2);

        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        assert_eq!(active_turn(&send).as_deref(), Some(turn_id.as_str()));
        assert_eq!(
            server.captured_requests().len(),
            2,
            "read-only queries must not call the provider"
        );
        let recovery = send
            .run_command(&["--json", "session", "recover", send.session_id()])
            .unwrap();
        evidence.record_command("explicit recovery of the cancelled attempt", &recovery);
        assert!(recovery.is_clean_exit(0), "{}", recovery.summary());
        assert_eq!(
            server.captured_requests().len(),
            2,
            "recovery may not start a provider request"
        );
        assert_eq!(
            fsynced_count(&effect_log),
            1,
            "recovery must not redispatch the mutation"
        );
        assert!(!active_turn_exists(&send));
        assert_eq!(
            std::fs::read_to_string(world.project_dir().join("cancelled-window.txt")).unwrap(),
            "kept\n",
            "the effect recorded before the crash must stay"
        );

        let log = send
            .run_command(&["--json", "session", "log", send.session_id()])
            .expect("session log");
        assert!(log.is_clean_exit(0));
        let log_json: Value = serde_json::from_slice(&log.stdout).expect("log JSON");
        let recovered = log_json["turns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|turn| turn["turn_id"] == json!(turn_id))
            .expect("exact crashed Turn");
        assert_eq!(recovered["origin"], "reconciled", "{recovered}");
        assert_eq!(
            recovered["outcome"],
            json!({"cancelled":{"reason":"user_cancelled"}}),
            "a persisted caller cancel must recover as top-level Cancelled: {recovered}"
        );
        evidence.record_command("cancelled recovery log", &log);

        let messages =
            committed_messages(&send, recovered["commit_id"].as_str().expect("commit id"));
        assert!(
            messages.iter().any(|message| message
                .to_string()
                .contains("cancel during the second model attempt")),
            "the user message must stay in the recovered history: {messages:?}"
        );
        assert!(
            messages.iter().any(|message| message
                .to_string()
                .contains("provider-private-cancelled-window-call")),
            "the completed Tool batch must stay in the recovered history: {messages:?}"
        );
        assert!(
            messages.iter().any(|message| message["role"] == "tool"),
            "the durable ToolResult must stay in the recovered history: {messages:?}"
        );
        evidence.record_captures(server.captured_requests());
    }));
    support::crash::finish(evidence, result);
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
    let mut evidence = support::evidence(&world, &spec);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        let server =
            wire_logged_fake_llm(&world, "dl-crash-matrix", sequence(scripts), &provider_log);
        let send = SessionSendDriver::new(&driver, &world, &spec, "dl-crash-matrix", "main")
            .expect("session");
        assert_clean_send(&send.send("baseline").expect("baseline")).expect("baseline clean");

        let ack = outside.path().join(format!("{}.json", case.barrier));
        world.set_env(BARRIER_ENV, case.barrier);
        world.set_env(ACK_ENV, ack.display().to_string());
        if let Some(injection) = case.injection() {
            world.set_env(INJECTION_ENV, injection);
        }
        let daemon = support::crash::start(&world, &spec, send.budget());
        let running = send.spawn("kill at durable barrier").expect("spawn send");
        let facts = wait_for_ack(&ack, send.budget().cap_timeout(Duration::from_secs(20)));
        assert_eq!(facts["barrier"], case.barrier);
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
        support::crash::kill(&world, daemon, running, &mut evidence, send.budget());
        world.unset_env(BARRIER_ENV);
        world.unset_env(ACK_ENV);
        world.unset_env(INJECTION_ENV);
        assert!(active_turn_exists(&send));
        assert_eq!(
            fsynced_count(&provider_log),
            expected_pre_kill,
            "queries must not recover"
        );
        let recovery = send
            .run_command(&["--json", "session", "recover", send.session_id()])
            .unwrap();
        evidence.record_command("independent crash recovery", &recovery);
        assert!(
            recovery.is_clean_exit(0),
            "recovery {}: {}",
            case.barrier,
            recovery.summary()
        );
        assert_eq!(
            server.captured_requests().len(),
            expected_pre_kill,
            "recovery may not start a provider request"
        );
        assert_eq!(fsynced_count(&provider_log), expected_pre_kill);
        assert!(!active_turn_exists(&send));
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
        assert_eq!(
            recovered_turns.len(),
            2,
            "baseline plus exactly one recovered Turn: {recovery_json}"
        );
        assert_eq!(recovered_turns[1]["turn_id"], session_facts["turn_id"]);
        if case.barrier == "turn_closing" {
            assert_eq!(recovered_turns[1]["origin"], "live");
            assert!(recovered_turns[1]["outcome"].get("completed").is_some());
        } else {
            let expected = case.expected_reconciled_outcome();
            assert_eq!(recovered_turns[1]["origin"], "reconciled");
            assert!(
                recovered_turns[1]["outcome"].get(expected).is_some(),
                "expected reconciled {expected}: {recovery_json}"
            );
        }
        assert!(
            recovered_turns
                .first()
                .is_some_and(|turn| turn["origin"].as_str() == Some("live")
                    && turn["outcome"].get("completed").is_some()),
            "{recovery_json}"
        );
        evidence.record_command("recovered log", &recovery_log);
        evidence.record_captures(server.captured_requests());
    }));
    support::crash::finish(evidence, result);
}
/// 当前活动 TurnId；崩溃遗留的 Turn 在恢复前保持可见，查询本身不触发恢复。
fn active_turn(send: &SessionSendDriver<'_>) -> Option<String> {
    let query = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            send.session_id(),
            "00000000-0000-4000-8000-000000000000",
        ])
        .unwrap();
    assert!(query.is_clean_exit(0), "{}", query.summary());
    let record: Value = serde_json::from_str(
        std::str::from_utf8(&query.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    record["view"]["active_turn"]["turn_id"]
        .as_str()
        .map(str::to_string)
}

fn active_turn_exists(send: &SessionSendDriver<'_>) -> bool {
    active_turn(send).is_some()
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

fn committed_messages(send: &SessionSendDriver<'_>, commit_id: &str) -> Vec<Value> {
    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .unwrap();
    assert!(log.is_clean_exit(0));
    let log: Value = serde_json::from_slice(&log.stdout).unwrap();
    let turn = log["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["commit_id"] == commit_id)
        .unwrap();
    let result = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            send.session_id(),
            turn["turn_id"].as_str().unwrap(),
        ])
        .unwrap();
    assert!(result.is_clean_exit(0));
    std::str::from_utf8(&result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["type"] == "transcript" && record["object_id"] == commit_id)
        .map(|record| record["message"].clone())
        .collect()
}

fn wait_for_ack(path: &std::path::Path, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        assert!(
            Instant::now() < deadline,
            "barrier ack exceeded scenario budget"
        );
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(facts) = serde_json::from_slice(&bytes)
        {
            return facts;
        }
        assert!(Instant::now() < deadline, "barrier ack was not published");
        std::thread::sleep(Duration::from_millis(5));
    }
}
