// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! provider 失败、SIGINT、deadline、文件不回滚与 Busy 的真实二进制场景。
mod support;
use lattice_system_harness::{
    AssertError, HarnessIntervention, ProcessTermination, SendExecution, SessionSendDriver,
    SseScript,
};
use serde_json::Value;
use std::time::Duration;
use support::*;

fn final_turn(outcome: &SendExecution) -> Value {
    live_turn(outcome)
}
fn failed(outcome: &SendExecution, cause: &str) -> Result<(), AssertError> {
    assert_clean_send(outcome)?;
    assert_accepted_and_closed(outcome)?;
    let turn = final_turn(outcome);
    let end = &turn["steps"].as_array().unwrap().last().unwrap()["end"];
    require(
        outcome.command.is_clean_exit(1)
            && turn["summary"]["outcome"] == "failed"
            && turn["summary"]["notice"]["level"] == "error",
        "Failed/Error with exit 1",
    )?;
    require(
        end["end"] == "stop" && end["cause"]["cause"] == cause,
        format!("Stop cause {cause}: {end}"),
    )
}
fn finish_failure(
    mut evidence: lattice_system_harness::ScenarioEvidence,
    send: &SessionSendDriver<'_>,
    commands: &[(&str, &lattice_system_harness::CommandOutcome)],
    captures: Vec<lattice_system_harness::CapturedRequest>,
    result: Result<(), AssertError>,
    test: &str,
) {
    for (label, command) in commands {
        evidence.record_command(*label, command);
    }
    evidence.record_captures(captures);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        result.unwrap();
        record_session(
            send,
            &mut evidence,
            send.session_id().unwrap(),
            serde_json::json!({"test":test,"commands":commands.iter().map(|(label, _)| *label).collect::<Vec<_>>()}),
        );
    }));
    finish_recorded(evidence, result);
}

fn provider_failure(name: &str, test: &str, script: SseScript, partial: bool) {
    let driver = driver();
    let (world, spec) = direct_world(
        name,
        "failure",
        "[]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(&world, "failure", script);
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "failure");
    let outcome = send.send("trigger failure").unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let result = (|| {
        failed(&outcome, "provider_error")?;
        require(server.captured_requests().len() == 1, "no provider retry")?;
        require(detail.is_clean_exit(0), "failed Turn remains readable")?;
        let persisted: Value = serde_json::from_slice(&detail.stdout).unwrap();
        require(
            persisted["steps"][0]["attempts"][0]["origin"]
                == serde_json::json!({"protocol":"chat-completions","provider_id":"openai-compatible"}),
            "failed attempt records protocol and provider id",
        )?;
        require(
            same_turn(&final_turn(&outcome), &persisted),
            "reloaded detail equals the live Step entries",
        )?;
        let entries = outcome.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.step().is_some())
            .expect("reserved Step persists even on failure");
        require(
            outcome.reply_text().is_empty() && entry.raw["content"]["reply"].is_null(),
            "failed attempt does not produce a complete reply",
        )?;
        // 停止尝试的半截文字不持久化；只从本次连接收到的实时文字证明输出。
        let view = outcome
            .view()
            .map_err(|e| AssertError::StdoutMissing { expected: e })?;
        let segments: Vec<_> = view.texts.values().flatten().collect();
        if partial {
            let text: String = segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect();
            require(
                !text.is_empty()
                    && "partial".starts_with(&text)
                    && segments
                        .iter()
                        .all(|segment| segment.kind == lattice_system_harness::SegmentKind::Text),
                format!("partial attempt must stream a nonempty prefix of partial: {segments:?}"),
            )
        } else {
            require(
                segments.is_empty(),
                "pre-output failure must have no text segments",
            )
        }
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("provider failure", &outcome.command),
            ("turn detail", &detail),
        ],
        server.captured_requests(),
        result,
        test,
    );
}
#[test]
fn direct_failure_http_error_no_retry_commits_error() {
    provider_failure(
        "direct-failure-http-error",
        "direct_failure_http_error_no_retry_commits_error",
        SseScript::HttpError {
            status: 500,
            body: r#"{"error":{"message":"provider down"}}"#.into(),
        },
        false,
    );
}
#[test]
fn direct_failure_disconnect_before_output_commits_error() {
    provider_failure(
        "direct-failure-disc-pre",
        "direct_failure_disconnect_before_output_commits_error",
        SseScript::Disconnect {
            chunks_before_drop: vec![],
        },
        false,
    );
}
#[test]
fn direct_failure_disconnect_after_partial_token_commits_error() {
    provider_failure(
        "direct-failure-disc-partial",
        "direct_failure_disconnect_after_partial_token_commits_error",
        SseScript::Disconnect {
            chunks_before_drop: vec!["partial".into()],
        },
        true,
    );
}
#[test]
fn direct_failure_malformed_sse_commits_error() {
    provider_failure(
        "direct-failure-malformed",
        "direct_failure_malformed_sse_commits_error",
        SseScript::IllegalResponse {
            body: "not json at all".into(),
        },
        false,
    );
}
#[test]
fn direct_failure_sigint_while_stalled_commits_cancelled() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-sigint-stall", "cancel", "[]", "");
    let (server, gate) = wire_gated_fake_llm(&world, "cancel", SseScript::plain("never-served"));
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "cancel");
    let mut running = send.spawn("stall then cancel").unwrap();
    gate.wait_until_stalled(1, running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    running.interrupt().unwrap();
    let outcome = send.wait_spawned(running).unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let result = (|| {
        require(
            outcome.command.termination == Some(ProcessTermination::Exited(2)),
            "cooperative cancellation exits 2",
        )?;
        require(
            outcome.command.interventions == vec![HarnessIntervention::InterruptRequested]
                && !outcome.command.cleanup.forced
                && outcome.command.cleanup.succeeded()
                && outcome.command.stdout_complete
                && outcome.command.stderr_complete,
            "SIGINT only, complete output and group cleanup",
        )?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome.malformed_lines.is_empty() && outcome.reply_text().is_empty(),
            "no malformed records or fabricated text",
        )?;
        let turn = final_turn(&outcome);
        require(
            turn["summary"]["outcome"] == "cancelled"
                && turn["summary"]["notice"]["level"] == "info"
                && turn["steps"][0]["end"]["cause"]["cause"] == "cancelled",
            "Cancelled/Info with durable cancellation cause",
        )?;
        require(
            detail.is_clean_exit(0)
                && same_turn(
                    &turn,
                    &serde_json::from_slice::<Value>(&detail.stdout).unwrap(),
                ),
            "cancellation survives reload",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("SIGINT", &outcome.command), ("reloaded Turn", &detail)],
        server.captured_requests(),
        result,
        "direct_failure_sigint_while_stalled_commits_cancelled",
    );
}
#[test]
fn direct_failure_profile_deadline_while_stalled_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-deadline-stall",
        "deadline",
        "[]",
        "[llm]\ntimeout_secs = 1\n",
    );
    let (server, _gate) = wire_gated_fake_llm(&world, "deadline", SseScript::plain("never-served"));
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "deadline");
    let outcome = send.send("stall then deadline").unwrap();
    let result = (|| {
        failed(&outcome, "deadline")?;
        require(
            outcome.reply_text().is_empty() && server.captured_requests().len() == 1,
            "one stalled request, no text",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("deadline", &outcome.command)],
        server.captured_requests(),
        result,
        "direct_failure_profile_deadline_while_stalled_commits_error",
    );
}
#[test]
fn direct_failure_mutation_survives_provider_failure() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-mutation-survives",
        "mutation",
        "[\"mutate__create\"]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "mutation",
        sequence(vec![
            tool_call(
                "c1",
                "mutate__create",
                r#"{"path":"survivor.txt","content":"bytes\n","create_parents":false}"#,
            ),
            SseScript::HttpError {
                status: 500,
                body: r#"{"error":{"message":"down"}}"#.into(),
            },
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "mutation");
    let outcome = send.send("create then fail").unwrap();
    let result = (|| {
        failed(&outcome, "provider_error")?;
        let turn = final_turn(&outcome);
        require(
            turn["steps"][0]["end"]["end"] == "continue"
                && turn["steps"][0]["slots"][0]["outcome"] == "succeeded",
            "mutation committed before failed next request",
        )?;
        require(
            std::fs::read_to_string(world.project_dir().join("survivor.txt")).unwrap() == "bytes\n",
            "installed bytes survive provider failure",
        )?;
        require(
            server.captured_requests().len() == 2,
            "tool round then failed request only",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("mutation then failure", &outcome.command)],
        server.captured_requests(),
        result,
        "direct_failure_mutation_survives_provider_failure",
    );
}
#[test]
fn direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-stale-parent", "busy", "[]", "");
    let baseline_server = wire_fake_llm(&world, "busy", SseScript::plain("baseline done"));
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "busy");
    let baseline = send.send("baseline").unwrap();
    let (gated, gate) = wire_gated_fake_llm(&world, "busy", SseScript::plain("A done"));
    let running = send.spawn("A").unwrap();
    gate.wait_until_stalled(1, running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    let racer_server = wire_fake_llm(&world, "busy", SseScript::plain("B done"));
    let racer = send.send("B").unwrap();
    let before_retry = racer_server.captured_requests();
    gate.release(running.remaining_timeout(Duration::from_secs(2)))
        .unwrap();
    let a = send.wait_spawned(running).unwrap();
    let b = send.send("B").unwrap();
    let log = send
        .run_command(&["--json", "session", "log", send.session_id().unwrap()])
        .unwrap();
    let result = (|| {
        for outcome in [&baseline, &a, &b] {
            assert_clean_send(outcome)?;
            assert_accepted_and_closed(outcome)?;
            require(
                outcome.has_completed() && outcome.command.is_clean_exit(0),
                "accepted Turn completes",
            )?;
        }
        assert_clean_send(&racer)?;
        require(
            racer.command.is_clean_exit(1)
                && racer.accepted_turn_id().is_none()
                && rejected_kind(&racer) == Some("busy"),
            "Busy rejects without acceptance",
        )?;
        require(
            before_retry.is_empty() && racer_server.captured_requests().len() == 1,
            "Busy never queues or calls provider",
        )?;
        let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
        let rows = tree["nodes"].as_array().unwrap();
        require(
            log.is_clean_exit(0) && rows.len() == 7,
            "root plus exactly three complete Turns",
        )?;
        let previous = final_turn(&a)["steps"][0]["node"].clone();
        require(
            rows.iter().any(|r| {
                r["turn_start"]["turn_id"] == b.accepted_turn_id().unwrap()
                    && r["node"]["parent"] == previous
            }),
            "B User is parented to A final node, not stale head",
        )
    })();
    let mut captures = baseline_server.captured_requests();
    captures.extend(gated.captured_requests());
    captures.extend(racer_server.captured_requests());
    finish_failure(
        evidence,
        &send,
        &[
            ("baseline", &baseline.command),
            ("Busy", &racer.command),
            ("A", &a.command),
            ("retry B", &b.command),
            ("tree", &log),
        ],
        captures,
        result,
        "direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry",
    );
}
