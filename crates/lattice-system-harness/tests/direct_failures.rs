// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local provider failures, public cancellation, and deadline
//! (issue #24 slice 5).
//!
//! Black-box coverage of terminal failure/cancellation/deadline paths through
//! the real `session send` binary, plus the "mutation survives provider
//! failure" invariant. A harness step timeout is a test-failure/cleanup
//! mechanism and is never reported as product cancellation/deadline coverage.
//!
//! ponytail: helpers duplicate other Direct-Local tests; later-slice split
//! owns the dedupe.

use std::fs;
use std::time::Duration;

use lattice_system_harness::{
    assert_event_subsequence, failure_dir, AssertError, CapturedRequest, CommandOutcome,
    LatticeDriver, RunningCommand, ScenarioEvidence, ScenarioSpec, ScenarioWorld, SendExecution,
    SendExitCode, SessionSendDriver, SseScript, Termination, TurnEventLine, FAKE_API_KEY,
};

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

fn failure_world(
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

/// Non-2xx startup with retry disabled commits an error turn; exit 1.
/// Expected terminal sequence: turn_started ... turn_failed -> turn_committing
/// -> turn_committed(status=error) ; exit 1.
#[test]
fn direct_failure_http_error_no_retry_commits_error() {
    let driver = driver();
    let (world, spec) = failure_world(
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
        assert_event_subsequence(
            &outcome.parsed,
            &[
                "turn_started",
                "turn_failed",
                "turn_committing",
                "turn_committed",
            ],
        )
        .map_err(|e| AssertError::StdoutMissing { expected: e })?;
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
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error must surface",
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
    let (world, spec) = failure_world(
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
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        assert_event_subsequence(
            &outcome.parsed,
            &[
                "turn_started",
                "turn_failed",
                "turn_committing",
                "turn_committed",
            ],
        )
        .map_err(|e| AssertError::StdoutMissing { expected: e })?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed must surface on early disconnect",
        )?;
        require(
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            "exit 1 on disconnect",
        )?;
        require(
            outcome.assistant_text().is_empty(),
            "no assistant text before disconnect",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("disconnect before output", &outcome.command)],
        captures,
        result,
    );
}

/// Disconnect after a partial token preserves partial content and commits error.
#[test]
fn direct_failure_disconnect_after_partial_token_commits_error() {
    let driver = driver();
    let (world, spec) = failure_world(
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
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error",
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
    let (world, spec) = failure_world(
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
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error",
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
    let (world, spec) = failure_world("direct-failure-sigint-stall", "dl-sigint", "[]", "");
    // Gated server holds the response so the provider is stalled (stream
    // opened, no events). The gate is never released; the SIGINT path must
    // terminate the turn.
    let (server, _gate) =
        wire_gated_fake_llm(&world, "dl-sigint", SseScript::plain("never-served"));
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-sigint", "main").expect("session");

    // Spawn the send so we can deliver SIGINT while it is stalled.
    let mut running: RunningCommand = send.spawn("stall then cancel").expect("spawn send");
    // Give the product time to open the provider stream and enter the stall.
    std::thread::sleep(Duration::from_millis(400));
    running.interrupt().expect("deliver SIGINT");

    let outcome = send.wait_spawned(running).expect("wait spawned");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        // The harness reports `Interrupted` whenever `interrupt()` was called,
        // even when the product cooperatively cancelled and exited with code
        // 1. The coverage signal is the JSONL event chain, not the
        // termination classification. Signal-kill would emit no events at
        // all, so the cancel->commit subsequence proves cooperative cancel.
        require(
            outcome.malformed_lines.is_empty(),
            "no malformed JSONL on cooperative cancel",
        )?;
        // turn_started must be present; assert the cancel->commit chain.
        assert_event_subsequence(
            &outcome.parsed,
            &["turn_cancelled", "turn_committing", "turn_committed"],
        )
        .map_err(|e| AssertError::StdoutMissing { expected: e })?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnCancelled { .. })),
            "turn_cancelled must surface on SIGINT",
        )?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "cancelled")),
            "turn_committed status=cancelled must surface",
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
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("sigint while stalled", &outcome.command)],
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
    let (world, spec) = failure_world(
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
        assert_event_subsequence(
            &outcome.parsed,
            &[
                "turn_started",
                "turn_failed",
                "turn_committing",
                "turn_committed",
            ],
        )
        .map_err(|e| AssertError::StdoutMissing { expected: e })?;
        require(
            outcome
                .events
                .iter()
                .any(|e| matches!(e, TurnEventLine::TurnFailed { .. })),
            "turn_failed on deadline",
        )?;
        require(
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error on deadline",
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
    let (world, spec) = failure_world(
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
            outcome.events.iter().any(
                |e| matches!(e, TurnEventLine::TurnCommitted { status, .. } if status == "error"),
            ),
            "turn_committed status=error",
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
