// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local provider failures, public cancellation, and deadline
//! (issue #46 (Codeberg #24) slice 5).
//!
//! Black-box coverage of terminal failure/cancellation/deadline paths through
//! the real `session send` binary, plus the "mutation survives provider
//! failure" invariant. A harness step timeout is a test-failure/cleanup
//! mechanism and is never reported as product cancellation/deadline coverage.

mod support;

use std::fs;
use std::time::Duration;

use lattice_system_harness::{
    AssertError, FAKE_API_KEY, FakeLlmServer, HarnessIntervention, ProcessTermination,
    RunningCommand, SendExitCode, SessionSendDriver, SseScript, TurnEventLine,
};
use serde_json::Value;
use support::{
    assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence, finish_scenario,
    require, sequence, tool_call, wire_fake_llm, wire_gated_fake_llm,
};

/// provider 非 2xx 且禁用重试时，接受的 Turn 持久关闭为失败并退出 1。
#[test]
fn direct_failure_http_error_no_retry_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-http-error",
        "dl-fail",
        "[]",
        "[llm]\ntimeout_secs = 30\n[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-fail",
        SseScript::HttpError {
            status: 500,
            body: r#"{"error":{"message":"provider down"}}"#.to_string(),
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-fail", "main").expect("session");

    let outcome = send.send("trigger startup failure").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome.events.iter().any(|e| {
                matches!(
                    e,
                    TurnEventLine::LlmRequestError {
                        will_retry: false,
                        ..
                    }
                )
            }),
            "llm_request_error with will_retry=false must surface",
        )?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed must surface",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "failed"),
            "fixed Turn outcome=failed",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            format!("exit code must be 1 (Failed), got {:?}", outcome.exit_code),
        )?;
        require(
            !outcome.has_committed_completed(),
            "no completed commit for a failed turn",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("http error no retry", &outcome.command)],
        captures,
        result,
    );
}

/// Disconnect before any output commits an error turn; exit 1.
#[test]
fn direct_failure_disconnect_before_output_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-disc-pre",
        "dl-disc-pre",
        "[]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-disc-pre",
        SseScript::Disconnect {
            chunks_before_drop: vec![],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-disc-pre", "main").expect("session");

    let outcome = send.send("trigger early disconnect").expect("turn");
    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log after early disconnect");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            !outcome
                .events
                .iter()
                .any(|event| matches!(event, TurnEventLine::CommitFailed { .. })),
            "stream failure must commit an error turn, not emit commit_failed",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "interrupted"),
            "fixed Turn outcome=interrupted",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on disconnect",
        )?;
        require(
            outcome.assistant_text().is_empty(),
            "no assistant text before disconnect",
        )?;
        let log_json: Value =
            serde_json::from_slice(&log.stdout).map_err(|error| AssertError::StdoutMissing {
                expected: format!("session log --json not JSON: {error}"),
            })?;
        let persisted = log_json
            .get("turns")
            .and_then(Value::as_array)
            .and_then(|turns| turns.last());
        require(
            persisted
                .and_then(|turn| turn.get("outcome"))
                .and_then(Value::as_object)
                .is_some_and(|outcome| outcome.contains_key("interrupted")),
            format!("stream failure was not durably logged as error: {log_json}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("disconnect before output", &outcome.command),
            ("session log after disconnect", &log),
        ],
        captures,
        result,
    );
}

/// Disconnect after a partial token preserves partial content and commits error.
#[test]
fn direct_failure_disconnect_after_partial_token_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-disc-partial",
        "dl-disc-partial",
        "[]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-disc-partial",
        SseScript::Disconnect {
            chunks_before_drop: vec!["partial".to_string()],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-disc-partial", "main").expect("session");

    let outcome = send.send("trigger mid-stream disconnect").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TokenDelta { content } if content == "partial"),
            ),
            "partial token must be preserved before disconnect",
        )?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed after mid-stream disconnect",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "interrupted"),
            "fixed Turn outcome=interrupted",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on mid-stream disconnect",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("disconnect after partial token", &outcome.command)],
        captures,
        result,
    );
}

/// Malformed SSE commits an error turn; exit 1.
#[test]
fn direct_failure_malformed_sse_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-malformed",
        "dl-malformed",
        "[]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-malformed",
        SseScript::IllegalResponse {
            body: "not json at all".to_string(),
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-malformed", "main").expect("session");

    let outcome = send.send("trigger malformed sse").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed on malformed SSE",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "interrupted"),
            "fixed Turn outcome=interrupted",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on malformed SSE",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("malformed sse", &outcome.command)],
        captures,
        result,
    );
}

/// SIGINT while the provider is stalled: cooperative cancellation commits a
/// cancelled turn; exit 1. The harness SIGINT (not SIGKILL) is the coverage
/// signal; a step timeout would be a test-failure cleanup, never reported here.
/// Expected sequence: turn_started ... turn_cancelled -> turn_committing ->
/// turn_committed(status=cancelled) ; exit 1.
#[test]
fn direct_failure_sigint_while_stalled_commits_cancelled() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-sigint-stall", "dl-sigint", "[]", "");
    // Gated server holds the response so the provider is stalled (stream
    // opened, no events). The gate is never released; the SIGINT path must
    // terminate the turn.
    let (server, gate) = wire_gated_fake_llm(&world, "dl-sigint", SseScript::plain("never-served"));
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-sigint", "main").expect("session");

    // Spawn the send, then prove the provider handler reached its intentional
    // stall before delivering SIGINT.
    let mut running: RunningCommand = send.spawn("stall then cancel").expect("spawn send");
    server
        .wait_for_requests(1, running.remaining_timeout(Duration::from_secs(60)))
        .expect("provider request before SIGINT");
    gate.wait_until_stalled(1, running.remaining_timeout(Duration::from_secs(2)))
        .expect("provider handler stalled before SIGINT");
    running.interrupt().expect("deliver SIGINT");

    let outcome = send.wait_spawned(running).expect("wait spawned");
    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log after cancellation");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        require(
            outcome.command.termination == Some(ProcessTermination::Exited(1)),
            format!(
                "cooperative cancellation must preserve exit 1: {}",
                outcome.command.summary()
            ),
        )?;
        require(
            outcome.command.interventions == vec![HarnessIntervention::InterruptRequested],
            format!(
                "cooperative cancellation must retain only SIGINT: {}",
                outcome.command.summary()
            ),
        )?;
        require(
            !outcome.command.cleanup.forced && outcome.command.cleanup.succeeded(),
            format!(
                "cooperative cancellation required forced cleanup: {:?}",
                outcome.command.cleanup
            ),
        )?;
        // The JSONL cancel-to-commit subsequence proves product cooperation;
        // a signal-killed process would emit no committed cancellation.
        require(
            outcome.malformed_lines.is_empty(),
            "no malformed JSONL on cooperative cancel",
        )?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome.events.iter().any(|event| {
                matches!(
                    event,
                    TurnEventLine::TurnCancelled { reason }
                        if reason.as_deref().is_some_and(|text| text.contains("cancelled"))
                )
            }),
            "turn_cancelled must carry a cancellation reason on SIGINT",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "cancelled"),
            "fixed Turn outcome=cancelled",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            format!(
                "exit code must be 1 (Failed) for cancelled turn, got {:?}",
                outcome.exit_code
            ),
        )?;
        require(
            !outcome.has_committed_completed(),
            "no completed commit for a cancelled turn",
        )?;
        require(
            outcome.assistant_text().is_empty(),
            "no assistant text while stalled",
        )?;
        let log_json: Value =
            serde_json::from_slice(&log.stdout).map_err(|error| AssertError::StdoutMissing {
                expected: format!("session log --json not JSON: {error}"),
            })?;
        require(
            log_json
                .get("turns")
                .and_then(Value::as_array)
                .and_then(|turns| turns.last())
                .and_then(|turn| turn.get("outcome"))
                .and_then(Value::as_object)
                .is_some_and(|outcome| outcome.contains_key("cancelled")),
            format!("cancelled turn was not durably logged: {log_json}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("sigint while stalled", &outcome.command),
            ("session log after cancellation", &log),
        ],
        captures,
        result,
    );
}

/// A one-second profile deadline while the provider is stalled commits an
/// error turn; exit 1. The deadline is a product control (not a harness step
/// timeout), so observing it counts as coverage.
#[test]
fn direct_failure_profile_deadline_while_stalled_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-deadline-stall",
        "dl-deadline",
        "[]",
        "[llm]\ntimeout_secs = 1\n",
    );
    let (server, _gate) =
        wire_gated_fake_llm(&world, "dl-deadline", SseScript::plain("never-served"));
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-deadline", "main").expect("session");

    let outcome = send.send("stall then deadline").expect("turn");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed on deadline",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "interrupted"),
            "fixed Turn outcome=interrupted on deadline",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on deadline",
        )?;
        require(
            outcome.assistant_text().is_empty(),
            "no assistant text while stalled before deadline",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("deadline while stalled", &outcome.command)],
        captures,
        result,
    );
}

/// A successful mutation followed by a provider failure on the next round
/// retains the Local bytes. This is the Direct-Local no-rollback invariant:
/// turn commits error, but the file written by the prior tool call persists.
#[test]
fn direct_failure_mutation_survives_provider_failure() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-mutation-survives",
        "dl-mut-surv",
        "[\"mutate.create\"]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "dl-mut-surv",
        sequence(vec![
            tool_call(
                "c1",
                "mutate.create",
                r#"{"path":"survivor.txt","content":"bytes\n","create_parents":false}"#,
            ),
            // Second round: the provider fails after the tool result is fed
            // back. The file must persist.
            SseScript::HttpError {
                status: 500,
                body: r#"{"error":{"message":"down"}}"#.to_string(),
            },
        ]),
    );
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-mut-surv", "main").expect("session");

    let outcome = send.send("create then fail").expect("turn");
    let captures = server.captured_requests();
    let survivor = world.project_dir().join("survivor.txt");

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        // The mutation tool call must have succeeded within the turn.
        require(
            outcome.events.iter().any(|e| matches!(
                e,
                TurnEventLine::ToolCallFinished { name, ok: true, .. } if name == "mutate.create"
            )),
            "mutate.create must finish ok before the provider failure",
        )?;
        // The turn terminates in error because the second round failed.
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed after the second-round provider failure",
        )?;
        require(
            outcome
                .committed_result()
                .is_some_and(|(_, _, result)| result == "failed"),
            "fixed Turn outcome=failed",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on mutation-survives failure turn",
        )?;
        // The invariant: the file written by the successful tool call persists
        // on Local even though the turn committed error (no rollback).
        require(
            survivor.is_file(),
            "survivor.txt must persist after the provider failure",
        )?;
        require(
            fs::read_to_string(&survivor).unwrap_or_default() == "bytes\n",
            "survivor.txt content must be unchanged after the failure",
        )?;
        Ok(())
    })();

    let _ = captures; // captures retained for evidence
    finish_scenario(
        scenario_evidence,
        &[("mutation survives failure", &outcome.command)],
        Vec::new(),
        result,
    );
}

/// 同 Session 忙时拒绝而不排队；请求必须显式重发，且以原 Turn 完成后的 head 为父。
#[test]
fn direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-stale-parent", "dl-stale", "[]", "");
    let baseline_server = wire_fake_llm(&world, "dl-stale", SseScript::plain("baseline done"));
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-stale", "main").expect("session");
    let baseline = send.send("baseline turn").expect("baseline");
    let (gated_server, gate) =
        wire_gated_fake_llm(&world, "dl-stale", SseScript::plain("stale turn tokens"));
    let running_a = send.spawn("stalled turn A").expect("spawn A");
    gate.wait_until_stalled(1, running_a.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    let racer_server = FakeLlmServer::start(SseScript::plain("racing commit")).unwrap();
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", racer_server.base_url());
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    let racer = send
        .send("racing turn B")
        .expect("busy B response before releasing A");
    let before_retry = racer_server.captured_requests();
    gate.release(running_a.remaining_timeout(Duration::from_secs(2)))
        .unwrap();
    let stalled = send.wait_spawned(running_a).expect("wait A");
    let retried = send.send("racing turn B").expect("explicit retry B");
    let log = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .unwrap();
    let mut captures = baseline_server.captured_requests();
    captures.extend(gated_server.captured_requests());
    captures.extend(racer_server.captured_requests());
    let result = (|| {
        for outcome in [&baseline, &stalled, &retried] {
            assert_clean_send(outcome)?;
            assert_accepted_and_closed(outcome)?;
            require(
                outcome.has_committed_completed() && outcome.command.is_clean_exit(0),
                "accepted turn must complete",
            )?;
        }
        require(
            racer.command.is_clean_exit(1) && racer.accepted_turn_id().is_none(),
            "Busy must reject without acceptance",
        )?;
        require(racer.events.iter().any(|event| matches!(event, TurnEventLine::Unknown { type_name, raw } if type_name == "error" && raw["kind"] == "busy")), "explicit Busy classification")?;
        require(
            before_retry.is_empty() && racer_server.captured_requests().len() == 1,
            "rejected B must not contact provider or queue",
        )?;
        let log_json: Value = serde_json::from_slice(&log.stdout).unwrap();
        let turns = log_json["turns"].as_array().unwrap();
        require(
            turns.len() == 3,
            "only baseline, A and explicit retry may commit",
        )?;
        let previous = stalled.committed_result().unwrap().1;
        require(
            retried.events.iter().any(|event| match event {
                TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => raw["view"]
                    ["requested_turn"]["parent_ids"]
                    .as_array()
                    .is_some_and(|parents| parents == &[Value::String(previous.into())]),
                _ => false,
            }),
            "explicit retry must commit against A, not its stale parent",
        )
    })();
    finish_scenario(
        scenario_evidence,
        &[
            ("baseline", &baseline.command),
            ("Busy B", &racer.command),
            ("A", &stalled.command),
            ("explicit B", &retried.command),
            ("log", &log),
        ],
        captures,
        result,
    );
}
