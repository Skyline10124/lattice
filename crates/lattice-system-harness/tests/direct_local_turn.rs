// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-Local turn scenarios (issue #62, Codeberg #40): the production turn
//! path runs against the canonical Local Workspace — no KVM, no MSB daemon,
//! no OCI image registry, no guest Git installation, no `[workspace]` profile
//! section. These scenarios run in the ordinary lane (not `#[ignore]`):
//! after #62 a broken turn path is a product failure, never a skip.
//!
//! Skeleton coverage (full Direct-Local scenario matrix belongs to #46,
//! Codeberg #24):
//!
//! - **plain-text turn** — a complete text-only turn commits and exits 0;
//!   `session log --json` cross-validates the commit (supersedes the retired
//!   KVM-lane plain-turn scenario with the same semantic core).
//! - **session reopen** — a second `session send` on the same session
//!   succeeds after an external (non-LATTICE) edit + commit changed the
//!   Local state, and carries the first turn's transcript context.
//! - **policy denial** — a `mutate.create` call on a
//!   `[lancet.workspace].denied_write_patterns` path is denied, surfaces as
//!   a failed tool event in the public stream, leaves no file on disk, and
//!   the turn still completes on the model's recovery reply.

mod support;

use lattice_system_harness::{
    AssertError, ScenarioSpec, ScenarioWorld, SendExitCode, SessionSendDriver, SseScript,
    TurnEventLine, assert_event_subsequence, derive_send_exit_code,
};
use support::{assert_clean_send, driver, evidence, finish_scenario, require, wire_fake_llm};

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
    let spec = ScenarioSpec::direct_local(prefix);
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

/// Minimal complete turn over the Direct-Local runtime: the fake LLM
/// receives a real Chat Completions request, `session send` emits the
/// required public event subsequence, the exit code is 0 and agrees with
/// the stream-derived code, and `session log --json` reflects the commit.
#[test]
fn direct_local_plain_text_turn_commits_and_exit_zero() {
    let driver = driver();
    let (world, spec) = turn_world("direct-local-plain-turn", "dl-plain", "[]", "");
    let server = wire_fake_llm(&world, "dl-plain", SseScript::plain("ok"));
    let scenario_evidence = evidence(&world, &spec);

    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-plain", "main").expect("create session");
    let execution = send.send("ping").expect("run session send");
    let outcome = &execution.parsed;
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log --json");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        assert_event_subsequence(
            outcome,
            &[
                "turn_started",
                "model_started",
                "token_delta",
                "model_finished",
                "turn_committed",
            ],
        )
        .map_err(|expected| AssertError::StdoutMissing { expected })?;

        let binary_exit = outcome
            .exit_code
            .expect("outcome should have an exit code classification");
        let derived = derive_send_exit_code(&outcome.events);
        require(
            binary_exit == SendExitCode::Committed && derived == SendExitCode::Committed,
            format!("exit code mismatch: binary={binary_exit:?} derived={derived:?}"),
        )?;

        let commit_id = outcome
            .events
            .iter()
            .find_map(|event| match event {
                TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.as_str()),
                _ => None,
            })
            .unwrap_or_default();
        let log_stdout = String::from_utf8_lossy(&log_output.stdout);
        require(
            log_output.is_clean_exit(0)
                && !commit_id.is_empty()
                && log_stdout.contains(commit_id)
                && log_stdout.contains("\"outcome\": {")
                && log_stdout.contains("\"completed\": {"),
            "session log --json did not reflect the new commit",
        )?;
        require(
            captures.iter().any(|capture| {
                capture.model == "dl-plain"
                    && capture.message_roles.iter().any(|role| role == "user")
            }),
            "fake LLM did not receive model=dl-plain with a user role",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("lattice --json session send <id> ping", &execution.command),
            ("lattice --json session log <id>", &log_output),
        ],
        captures,
        result,
    );
}

/// Session reopen against changed Local state: an external (non-LATTICE)
/// edit + commit lands between two turns of the same session; the second
/// turn still commits and its request carries the first turn's transcript
/// (user text + assistant reply), proving the reopen path reads current
/// Local state and session history. Skeleton — full external-binary
/// competition coverage (mid-turn races, mutation conflicts) is #46.
#[test]
fn direct_local_session_reopen_after_external_change() {
    let driver = driver();
    let (world, spec) = turn_world("direct-local-reopen", "dl-reopen", "[]", "");
    let server = wire_fake_llm(
        &world,
        "dl-reopen",
        SseScript::Sequence {
            scripts: vec![SseScript::plain("pong-one"), SseScript::plain("pong-two")],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-reopen", "main")
        .expect("create session");

    let first = send.send("ping").expect("first turn drives to completion");
    world
        .write_project_file("external.txt", "external edit between turns\n")
        .expect("external file");
    world.git(&["add", "external.txt"]).expect("git add");
    world
        .git(&["commit", "-m", "external change"])
        .expect("git commit");
    let second = send
        .send("again")
        .expect("second turn drives to completion");
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log --json");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(
            first.has_committed_completed(),
            format!("first turn did not commit: {:?}", first.type_sequence()),
        )?;
        require(
            second.has_committed_completed(),
            format!(
                "second turn did not commit after external change: {:?}",
                second.type_sequence()
            ),
        )?;
        require(
            captures.len() == 2,
            format!("expected one LLM request per turn, got: {captures:?}"),
        )?;
        let second_body = captures
            .get(1)
            .map(|capture| capture.raw.to_string())
            .unwrap_or_default();
        require(
            second_body.contains("ping") && second_body.contains("pong-one"),
            format!("second request lacked first-turn transcript context: {second_body}"),
        )?;

        let commit_ids: Vec<&str> = [&first, &second]
            .iter()
            .filter_map(|outcome| {
                outcome.events.iter().find_map(|event| match event {
                    TurnEventLine::TurnCommitted { commit_id, .. } => Some(commit_id.as_str()),
                    _ => None,
                })
            })
            .collect();
        let log_stdout = String::from_utf8_lossy(&log_output.stdout);
        require(commit_ids.len() == 2, "both turns must report commit ids")?;
        require(
            commit_ids.iter().all(|id| log_stdout.contains(id)),
            format!("session log omitted a commit: {log_stdout}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("first session send", &first.command),
            ("second session send", &second.command),
            ("session log", &log_output),
        ],
        captures,
        result,
    );
}

/// Policy denial over the Direct-Local runtime: `mutate.create` targeting a
/// `[lancet.workspace].denied_write_patterns` path is rejected by the
/// guarded Local Workspace, surfaces as a failed tool event in the public
/// stream, and leaves nothing on disk; the model's recovery reply completes
/// the turn. Skeleton — the full denial matrix (read denial, effect denial,
/// system exclusions) is #46.
#[test]
fn direct_local_mutate_create_denied_by_write_policy() {
    let driver = driver();
    let (world, spec) = turn_world(
        "direct-local-policy-denial",
        "dl-deny",
        "[\"mutate.create\"]",
        "\n[lancet.workspace]\ndenied_write_patterns = [\"blocked/**\"]\n",
    );
    let server = wire_fake_llm(
        &world,
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
    let scenario_evidence = evidence(&world, &spec);
    let send =
        SessionSendDriver::new(&driver, &world, &spec, "dl-deny", "main").expect("create session");
    let outcome = send.send("create blocked/denied.txt").expect("turn drives");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&outcome)?;
        let started = outcome.events.iter().any(|event| {
            matches!(
                event,
                TurnEventLine::ToolCallStarted { name, .. } if name == "mutate.create"
            )
        });
        let failed_finish = outcome.events.iter().any(|event| {
            matches!(
                event,
                TurnEventLine::ToolCallFinished { name, ok: false, .. }
                    if name == "mutate.create"
            )
        });
        require(
            started && failed_finish,
            format!(
                "stream lacked failed mutate.create lifecycle: {:?}",
                outcome.events
            ),
        )?;
        require(
            !world.project_dir().join("blocked/denied.txt").exists(),
            "denied mutate.create left a file on disk",
        )?;
        require(
            outcome.has_committed_completed(),
            format!(
                "turn did not recover after denial: {:?}",
                outcome.type_sequence()
            ),
        )?;
        require(
            captures.len() == 2,
            format!("expected Tool round + recovery round, got: {captures:?}"),
        )?;
        let second_body = captures
            .get(1)
            .map(|capture| capture.raw.to_string())
            .unwrap_or_default();
        require(
            second_body.contains("denied") || second_body.contains("tool"),
            format!("second request lacked Tool denial result: {second_body}"),
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[("denied mutate.create turn", &outcome.command)],
        captures,
        result,
    );
}
