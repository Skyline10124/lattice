// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local turn scenarios (issue #40): the production turn path runs
//! against the canonical Local Workspace — no KVM, no MSB daemon, no OCI
//! image registry, no guest Git installation, no `[workspace]` profile
//! section. These scenarios run in the ordinary lane (not `#[ignore]`):
//! after #40 a broken turn path is a product failure, never a skip.
//!
//! Skeleton coverage (full Direct-Local scenario matrix belongs to #24):
//!
//! - **plain-text turn** — a complete text-only turn commits and exits 0;
//!   `session log --json` cross-validates the commit (supersedes the retired
//!   MSB-lane `msb_full_turn.rs` scenario with the same semantic core).
//! - **session reopen** — a second `session send` on the same session
//!   succeeds after an external (non-LATTICE) edit + commit changed the
//!   Local state, and carries the first turn's transcript context.
//! - **policy denial** — a `mutate.create` call on a
//!   `[lancet.workspace].denied_write_patterns` path is denied, surfaces as
//!   a failed tool event in the public stream, leaves no file on disk, and
//!   the turn still completes on the model's recovery reply.

#![cfg(unix)]

use std::process::Output;

use lattice_system_harness::{
    AssertError, LatticeCommand, LatticeDriver, ScenarioSpec, ScenarioWorld, SendExitCode,
    SessionSendDriver, SseScript, TurnEventLine, assert_event_subsequence,
    assert_with_failure_artifacts, derive_send_exit_code, parse_send_stdout,
};

fn driver() -> LatticeDriver {
    LatticeDriver::from_env().expect("LATTICE_BIN must point to a built lattice binary")
}

/// A Direct-Local project world with a fake-LLM-backed agent profile.
///
/// `tools` is the `[tools].enabled` list; `extra_profile` is appended verbatim
/// (e.g. a `[lancet.workspace]` policy table). The profile is hand-written
/// TOML — the harness boundary forbids `lattice-profile`.
fn turn_world(
    prefix: &str,
    agent: &str,
    tools: &str,
    extra_profile: &str,
) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::fast(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("world");
    let toml = format!(
        "schema_version = 1\n\n[agent]\nname = \"{agent}\"\nmodel = \"compat:{agent}\"\n\n[prompt]\nsystem = \"You are a concise test agent. Reply briefly.\"\n\n[tools]\nenabled = {tools}\n{extra_profile}"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("write agent.toml");
    world.init_git_project().expect("init git project");
    (world, spec)
}

/// Point the world at a fake LLM through the formal `OPENAI_COMPATIBLE_*`
/// configuration (RFC #19 decision four: no test-only provider).
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

/// Drive `lattice --json session send` directly so the raw `Output` stays
/// available for failure artifacts.
fn raw_send(
    driver: &LatticeDriver,
    world: &ScenarioWorld,
    spec: &ScenarioSpec,
    session_id: &str,
    prompt: &str,
) -> Output {
    driver
        .run(
            world,
            &LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "send".into(),
                    session_id.into(),
                    prompt.into(),
                ],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("run session send")
}

/// Minimal complete turn over the Direct-Local runtime: the fake LLM
/// receives a real Chat Completions request, `session send` emits the
/// required public event subsequence, the exit code is 0 and agrees with
/// the stream-derived code, and `session log --json` reflects the commit.
#[test]
fn direct_local_plain_text_turn_commits_and_exit_zero() {
    let driver = driver();
    let (mut world, spec) = turn_world("direct-local-plain-turn", "dl-plain", "[]", "");
    let server = wire_fake_llm(&mut world, "dl-plain", SseScript::plain("ok"));

    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-plain", "main").expect("create session");

    let send_output = raw_send(&driver, &world, &spec, send.session_id(), "ping");
    let outcome = parse_send_stdout(&send_output.stdout, send_output.status.code());

    // Cross-validate via the public session log JSON.
    let log_output = driver
        .run(
            &world,
            &LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "log".into(),
                    send.session_id().into(),
                ],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("session log --json");

    let required = [
        "turn_started",
        "model_started",
        "token_delta",
        "model_finished",
        "turn_committed",
    ];
    let subseq = assert_event_subsequence(&outcome, &required);

    let binary_exit = outcome
        .exit_code
        .expect("outcome should have an exit code classification");
    let derived = derive_send_exit_code(&outcome.events);
    let exit_ok = binary_exit == SendExitCode::Committed && derived == SendExitCode::Committed;

    let log_stdout = String::from_utf8_lossy(&log_output.stdout).to_string();
    let send_commit_id = outcome
        .events
        .iter()
        .find_map(|e| match e {
            TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let log_ok = log_output.status.success()
        && !send_commit_id.is_empty()
        && log_stdout.contains(&send_commit_id)
        && log_stdout.contains("\"status\": \"completed\"");

    let caps = server.captured_requests();
    let fake_llm_ok = caps
        .iter()
        .any(|c| c.model == "dl-plain" && c.message_roles.iter().any(|r| r == "user"));

    let result: Result<(), AssertError> = (|| {
        subseq.map_err(|e| AssertError::StdoutMissing { expected: e })?;
        if !exit_ok {
            return Err(AssertError::StdoutMissing {
                expected: format!("exit code mismatch: binary={binary_exit:?} derived={derived:?}"),
            });
        }
        if !log_ok {
            return Err(AssertError::StdoutMissing {
                expected: "session log --json did not reflect the new commit".into(),
            });
        }
        if !fake_llm_ok {
            return Err(AssertError::StdoutMissing {
                expected: "fake LLM did not receive a request with model=dl-plain and a user role"
                    .into(),
            });
        }
        Ok(())
    })();

    assert_with_failure_artifacts(
        &world,
        &spec.name,
        "lattice --json session send <id> ping",
        &send_output,
        &[lattice_system_harness::FAKE_API_KEY],
        result,
    )
    .unwrap_or_else(|e| panic!("{e}"));
}

/// Session reopen against changed Local state: an external (non-LATTICE)
/// edit + commit lands between two turns of the same session; the second
/// turn still commits and its request carries the first turn's transcript
/// (user text + assistant reply), proving the reopen path reads current
/// Local state and session history. Skeleton — full external-binary
/// competition coverage (mid-turn races, mutation conflicts) is #24.
#[test]
fn direct_local_session_reopen_after_external_change() {
    let driver = driver();
    let (mut world, spec) = turn_world("direct-local-reopen", "dl-reopen", "[]", "");
    let server = wire_fake_llm(
        &mut world,
        "dl-reopen",
        SseScript::Sequence {
            scripts: vec![SseScript::plain("pong-one"), SseScript::plain("pong-two")],
        },
    );

    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-reopen", "main")
        .expect("create session");

    let first = send.send("ping").expect("first turn drives to completion");
    assert!(
        first.has_committed_completed(),
        "first turn must commit completed, events: {:?}",
        first.type_sequence()
    );

    // External, non-LATTICE change to the Local Workspace between turns:
    // a new tracked file plus a git commit the session did not make.
    world
        .write_project_file("external.txt", "external edit between turns\n")
        .expect("external file");
    world.git(&["add", "external.txt"]).expect("git add");
    world
        .git(&["commit", "-m", "external change"])
        .expect("git commit");

    // Reopen the same session for a second turn against the changed state.
    let second = send
        .send("again")
        .expect("second turn drives to completion");
    assert!(
        second.has_committed_completed(),
        "second turn must commit completed after external change, events: {:?}",
        second.type_sequence()
    );

    // Transcript continuity: the second request must carry the first turn's
    // user prompt and assistant reply.
    let caps = server.captured_requests();
    assert_eq!(
        caps.len(),
        2,
        "expected exactly one LLM request per turn, got: {caps:?}"
    );
    let second_body = caps[1].raw.to_string();
    assert!(
        second_body.contains("ping") && second_body.contains("pong-one"),
        "second turn request must include first-turn transcript context; body: {second_body}"
    );

    // `session log --json` lists both commits in order.
    let log_output = driver
        .run(
            &world,
            &LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "log".into(),
                    send.session_id().into(),
                ],
                timeout: Some(spec.step_timeout()),
            },
        )
        .expect("session log --json");
    let log_stdout = String::from_utf8_lossy(&log_output.stdout).to_string();
    let commit_ids: Vec<String> = [first, second]
        .iter()
        .filter_map(|outcome| {
            outcome.events.iter().find_map(|e| match e {
                TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.clone()),
                _ => None,
            })
        })
        .collect();
    assert_eq!(commit_ids.len(), 2, "both turns must report commit ids");
    for id in &commit_ids {
        assert!(
            log_stdout.contains(id),
            "session log must list commit {id}; log: {log_stdout}"
        );
    }
}

/// Policy denial over the Direct-Local runtime: `mutate.create` targeting a
/// `[lancet.workspace].denied_write_patterns` path is rejected by the
/// guarded Local Workspace, surfaces as a failed tool event in the public
/// stream, and leaves nothing on disk; the model's recovery reply completes
/// the turn. Skeleton — the full denial matrix (read denial, effect denial,
/// system exclusions) is #24.
#[test]
fn direct_local_mutate_create_denied_by_write_policy() {
    let driver = driver();
    let (mut world, spec) = turn_world(
        "direct-local-policy-denial",
        "dl-deny",
        "[\"mutate.create\"]",
        "\n[lancet.workspace]\ndenied_write_patterns = [\"blocked/**\"]\n",
    );
    let server = wire_fake_llm(
        &mut world,
        "dl-deny",
        SseScript::Sequence {
            scripts: vec![
                SseScript::ToolCall {
                    call_id: "call_1".into(),
                    tool_name: "mutate.create".into(),
                    arguments_json:
                        r#"{"path":"blocked/denied.txt","content":"must not land","create_parents":true}"#
                            .into(),
                },
                SseScript::plain("the write was denied; leaving the tree unchanged"),
            ],
        },
    );

    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-deny", "main").expect("create session");
    let outcome = send.send("create blocked/denied.txt").expect("turn drives");

    // The denied call surfaces as started + failed in the public stream.
    let started = outcome.events.iter().any(|e| {
        matches!(
            e,
            TurnEventLine::ToolCallStarted { name, .. } if name == "mutate.create"
        )
    });
    let failed_finish = outcome.events.iter().any(|e| {
        matches!(
            e,
            TurnEventLine::ToolCallFinished { name, ok: false, .. } if name == "mutate.create"
        )
    });
    assert!(
        started && failed_finish,
        "stream must show mutate.create started and finished with ok=false; events: {:?}",
        outcome.events
    );

    // The guarded Local Workspace wrote nothing.
    assert!(
        !world.project_dir().join("blocked/denied.txt").exists(),
        "denied mutate.create must not leave a file on disk"
    );

    // The turn recovers on the model's second reply and commits completed.
    assert!(
        outcome.has_committed_completed(),
        "turn must commit completed after the denial, events: {:?}",
        outcome.type_sequence()
    );

    // Exactly two LLM requests: the tool-call round and the recovery round.
    let caps = server.captured_requests();
    assert_eq!(
        caps.len(),
        2,
        "expected tool-call round + recovery round, got: {caps:?}"
    );
    // The denial result is fed back to the model as a tool message.
    let second_body = caps[1].raw.to_string();
    assert!(
        second_body.contains("denied") || second_body.contains("tool"),
        "second request must carry the tool denial result; body: {second_body}"
    );
}
