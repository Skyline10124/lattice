// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local completed-Turn contract (issue #46 (Codeberg #24) slice 1):
//! one vertical external-binary scenario over two turns of one session,
//! asserting only the public surface — the `session send` JSONL stream, the
//! captured Chat Completions requests, and `session log/status --json`.
//!
//! Observable assertions (separate named blocks below, one shared world):
//!
//! - **stream contract** — first event `turn_started`; token/model activity
//!   before `turn_committing`/`turn_committed`; started/committed session IDs
//!   match; nonempty commit ID; `completed` status; no failed/cancelled/
//!   commit-failed events; branch populated on `turn_started`.
//! - **routed provider/model** — `model_started` carries the routed provider
//!   (`openai_compatible`) and profile model; the fake LLM receives the
//!   provider model on the wire. A separate scenario covers default `sonnet`
//!   → Anthropic routing without contacting `api.anthropic.com`.
//! - **message assembly / reopen context** — request one is system + current
//!   user input; request two contains exactly the prior user/assistant pair
//!   before the new user message.
//! - **log/status** — JSON reports both commit IDs, count two, HEAD `main`; the
//!   first turn's log entry is unchanged after the second delta commit; human
//!   log output renders the committed turn.
//! - **missing session** — `session send` on an unknown session exits 2 with
//!   no JSONL Turn events on stdout.
//!
//! No full event golden: unknown additive events are allowed between the
//! required events (RFC #41 (Codeberg #19) decision five).

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, ScenarioSpec, ScenarioWorld, SendExitCode, SendOutcome,
    SessionSendDriver, SseScript, TurnEventLine, assert_event_subsequence, derive_send_exit_code,
    parse_send_stdout,
};
use serde_json::Value;
use support::{assert_clean_send, driver, evidence, finish_scenario, require, wire_fake_llm};

/// A Direct-Local project world with a fake-LLM-backed agent profile.
fn turn_world(prefix: &str, agent: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n[agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n[prompt]\nsystem = \"You are a concise test agent. Reply briefly.\"\n\n[tools]\nenabled = []\n"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");
    (world, spec)
}

/// `(role, content)` pairs of a captured Chat Completions request, in wire
/// order. Non-string content (e.g. part arrays) is compared by its JSON text.
fn message_parts(capture: &CapturedRequest) -> Vec<(String, String)> {
    capture
        .raw
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .map(|message| {
                    let role = message
                        .get("role")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let content = match message.get("content") {
                        Some(Value::String(text)) => text.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    (role, content)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assertion block: completed-Turn stream contract for one `session send`.
fn assert_completed_turn(outcome: &SendOutcome, session_id: &str) -> Result<(), AssertError> {
    // First event is turn_started; branch is populated.
    match outcome.events.first() {
        Some(TurnEventLine::TurnStarted {
            session_id: started,
            turn_id: _,
            branch,
        }) => {
            require(
                started == session_id,
                format!("turn_started session_id {started} != {session_id}"),
            )?;
            require(
                branch.as_deref() == Some("main"),
                format!("turn_started branch not populated: {branch:?}"),
            )?;
        }
        other => {
            return Err(AssertError::StdoutMissing {
                expected: format!("first event must be turn_started, got: {other:?}"),
            });
        }
    }

    // Token/model activity before turn_committing and turn_committed.
    assert_event_subsequence(
        outcome,
        &[
            "turn_started",
            "model_started",
            "token_delta",
            "model_finished",
            "turn_committing",
            "turn_committed",
        ],
    )
    .map_err(|expected| AssertError::StdoutMissing { expected })?;

    // Routed provider/model on model_started.
    let (provider, model) = outcome
        .events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::ModelStarted { provider, model } => Some((provider, model)),
            _ => None,
        })
        .expect("subsequence guarantees model_started");
    require(
        provider.as_deref() == Some("openai_compatible"),
        format!("model_started provider not routed: {provider:?}"),
    )?;
    require(
        model == "compat:dl-contract",
        format!("model_started model mismatch: {model}"),
    )?;

    // Matching started/committed session IDs, nonempty commit ID, completed.
    let (committed_session, commit_id, status) = outcome
        .events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::TurnCommitted {
                session_id,
                commit_id,
                status,
            } => Some((session_id, commit_id, status)),
            _ => None,
        })
        .expect("subsequence guarantees turn_committed");
    require(
        committed_session == session_id && !commit_id.is_empty() && status == "completed",
        format!(
            "bad turn_committed: session={committed_session} commit={commit_id} status={status}"
        ),
    )?;

    // No failed/cancelled/commit-failed events on success.
    require(
        !outcome.events.iter().any(|event| {
            matches!(
                event,
                TurnEventLine::TurnFailed { .. }
                    | TurnEventLine::TurnCancelled { .. }
                    | TurnEventLine::CommitFailed { .. }
            )
        }),
        format!("failure event on successful turn: {:?}", outcome.events),
    )?;

    // Binary exit code agrees with the stream-derived code.
    let binary_exit = outcome
        .exit_code
        .expect("outcome should have an exit code classification");
    let derived = derive_send_exit_code(&outcome.events);
    require(
        binary_exit == SendExitCode::Committed && derived == SendExitCode::Committed,
        format!("exit code mismatch: binary={binary_exit:?} derived={derived:?}"),
    )?;
    Ok(())
}

/// The commit ID a completed turn reported, for log/status cross-checks.
fn committed_id(outcome: &SendOutcome) -> String {
    outcome
        .events
        .iter()
        .find_map(|event| match event {
            TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.clone()),
            _ => None,
        })
        .expect("turn_committed present")
}

#[test]
fn direct_turn_completed_reopen_contract() {
    let driver = driver();
    let (world, spec) = turn_world("direct-turn-contract", "dl-contract");
    let server = wire_fake_llm(
        &world,
        "dl-contract",
        SseScript::Sequence {
            scripts: vec![
                SseScript::plain("first answer"),
                SseScript::plain("second answer"),
            ],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-contract", "main")
        .expect("create session");
    let session_id = send.session_id().to_string();
    let sid = session_id.as_str();

    let first = send.send("first prompt").expect("first turn drives");
    let log_after_first = send
        .run_command(&["--json", "session", "log", sid])
        .expect("session log after first turn");
    let second = send.send("second prompt").expect("second turn drives");
    let log_after_second = send
        .run_command(&["--json", "session", "log", sid])
        .expect("session log after second turn");
    let human_log = send
        .run_command(&["session", "log", sid])
        .expect("human session log after second turn");
    let status_output = send
        .run_command(&["--json", "session", "status", sid])
        .expect("session status --json");
    let missing = send
        .run_command(&["--json", "session", "send", "missing-session-id", "hello"])
        .expect("missing-session send drives");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        // --- stream contract: both turns complete with the required shape ---
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        assert_completed_turn(&first.parsed, &session_id)?;
        assert_completed_turn(&second.parsed, &session_id)?;

        // --- message assembly: request one is system + current user input ---
        require(
            captures.len() == 2,
            format!("expected one LLM request per turn, got: {captures:?}"),
        )?;
        let first_parts = message_parts(&captures[0]);
        require(
            captures[0].model == "dl-contract"
                && first_parts.len() == 2
                && first_parts[0].0 == "system"
                && first_parts[1].0 == "user"
                && first_parts[1].1.contains("first prompt"),
            format!("first request not system+user with current input: {first_parts:?}"),
        )?;

        // --- reopen context: request two carries exactly the prior
        // user/assistant pair before the new user message ---
        let second_parts = message_parts(&captures[1]);
        let roles: Vec<&str> = second_parts.iter().map(|(role, _)| role.as_str()).collect();
        require(
            roles == ["system", "user", "assistant", "user"],
            format!("second request roles not transcript+input: {roles:?}"),
        )?;
        require(
            second_parts[1].1 == "first prompt"
                && second_parts[2].1 == "first answer"
                && second_parts[3].1 == "second prompt",
            format!("second request lacked exact prior pair before new input: {second_parts:?}"),
        )?;

        // --- log/status: both commit IDs, count two, HEAD main ---
        let log_stdout = String::from_utf8_lossy(&log_after_second.stdout);
        let log_json: Value = serde_json::from_str(log_stdout.trim()).map_err(|error| {
            AssertError::StdoutMissing {
                expected: format!("session log --json not JSON: {error}"),
            }
        })?;
        let turns = log_json
            .get("turns")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let first_commit = committed_id(&first.parsed);
        let second_commit = committed_id(&second.parsed);
        let logged_ids: Vec<&str> = turns
            .iter()
            .filter_map(|turn| turn.get("commit_id").and_then(Value::as_str))
            .collect();
        require(
            turns.len() == 2
                && logged_ids.contains(&first_commit.as_str())
                && logged_ids.contains(&second_commit.as_str()),
            format!("session log did not report both commits: {logged_ids:?}"),
        )?;
        let human_log_stdout = String::from_utf8_lossy(&human_log.stdout);
        require(
            human_log.status.success()
                && human_log_stdout.contains("turn 0")
                && human_log_stdout.contains("first prompt"),
            format!("human session log omitted committed turn: {human_log_stdout}"),
        )?;

        let status_stdout = String::from_utf8_lossy(&status_output.stdout);
        let status_json: Value = serde_json::from_str(status_stdout.trim()).map_err(|error| {
            AssertError::StdoutMissing {
                expected: format!("session status --json not JSON: {error}"),
            }
        })?;
        require(
            status_json.get("commit_count").and_then(Value::as_u64) == Some(2),
            format!("status commit_count != 2: {status_json}"),
        )?;
        require(
            status_json
                .pointer("/head/branch/name")
                .and_then(Value::as_str)
                == Some("main"),
            format!("status HEAD not branch main: {status_json}"),
        )?;

        // --- first-turn content unchanged after the second delta commit ---
        let first_log_stdout = String::from_utf8_lossy(&log_after_first.stdout);
        let first_log_json: Value =
            serde_json::from_str(first_log_stdout.trim()).map_err(|error| {
                AssertError::StdoutMissing {
                    expected: format!("first session log --json not JSON: {error}"),
                }
            })?;
        let first_entry_before = first_log_json
            .get("turns")
            .and_then(Value::as_array)
            .and_then(|turns| turns.first())
            .cloned();
        require(
            first_entry_before.is_some() && first_entry_before == turns.first().cloned(),
            "first-turn log entry changed after the second commit",
        )?;

        // --- missing session: exit 2 with no JSONL Turn events ---
        let missing_outcome = parse_send_stdout(&missing.stdout, missing.status.code());
        require(
            missing_outcome.exit_code == Some(SendExitCode::InputError),
            format!(
                "missing session did not exit 2: code={:?} termination={:?}",
                missing.status.code(),
                missing.termination
            ),
        )?;
        require(
            missing_outcome.events.is_empty(),
            format!(
                "missing session emitted Turn events: {:?}",
                missing_outcome.events
            ),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("first session send", &first.command),
            ("session log after first turn", &log_after_first),
            ("second session send", &second.command),
            ("session log after second turn", &log_after_second),
            ("human session log after second turn", &human_log),
            ("session status --json", &status_output),
            ("missing-session send", &missing),
        ],
        captures,
        result,
    );
}

/// Default alias `sonnet` routes to Anthropic on the public `model_started`
/// event. The agent loop emits that event before provider I/O, so missing
/// `ANTHROPIC_API_KEY` still surfaces the routed provider without contacting
/// `api.anthropic.com`, then fails closed on the credential-class terminal.
#[test]
fn direct_turn_sonnet_routed_provider_event() {
    let driver = driver();
    let spec = ScenarioSpec::direct_local("direct-turn-sonnet-route");
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = "schema_version = 1\n\n\
[agent]\nname = \"dl-sonnet\"\nmodel = \"sonnet\"\n\n\
[prompt]\nsystem = \"You are a concise test agent.\"\n\n\
[tools]\nenabled = []\n\n\
[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n";
    world
        .write_project_file(".lattice/agents/dl-sonnet/agent.toml", toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");

    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-sonnet", "main").expect("session");
    let outcome = send.send("route sonnet").expect("turn");

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        let model_started = outcome.events.iter().find_map(|event| match event {
            TurnEventLine::ModelStarted { provider, model } => {
                Some((provider.clone(), model.clone()))
            }
            _ => None,
        });
        require(
            model_started.is_some(),
            format!(
                "model_started missing before provider failure: {:?}",
                outcome.type_sequence()
            ),
        )?;
        let (provider, model) = model_started.expect("checked");
        require(
            provider.as_deref() == Some("anthropic"),
            format!("sonnet must route provider=anthropic, got {provider:?}"),
        )?;
        require(
            model == "sonnet",
            format!("sonnet model_started model mismatch: {model}"),
        )?;
        let credential_terminal = outcome.events.iter().any(|event| match event {
            TurnEventLine::LlmRequestError { message, .. } => {
                let lower = message.to_lowercase();
                lower.contains("api key")
                    || lower.contains("credential")
                    || lower.contains("anthropic_api_key")
            }
            TurnEventLine::TurnFailed { reason } => {
                let lower = reason.to_lowercase();
                lower.contains("api key")
                    || lower.contains("credential")
                    || lower.contains("anthropic_api_key")
            }
            _ => false,
        });
        require(
            credential_terminal,
            format!(
                "missing ANTHROPIC_API_KEY must surface credential-class terminal: {:?}",
                outcome.type_sequence()
            ),
        )?;
        require(
            !outcome.events.iter().any(|event| {
                matches!(
                    event,
                    TurnEventLine::TurnCommitted { status, .. } if status == "completed"
                )
            }),
            "credential failure must not complete the turn",
        )?;
        require(
            outcome.exit_code == Some(SendExitCode::Failed),
            format!(
                "credential failure exit must be Failed, got {:?}",
                outcome.exit_code
            ),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("sonnet routed model_started", &outcome.command)],
        Vec::new(),
        result,
    );
}
