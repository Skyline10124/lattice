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
    AssertError, CapturedRequest, ScenarioSpec, ScenarioWorld, SendExitCode, SendOutcome,
    SessionSendDriver, SseScript, TurnEventLine, derive_send_exit_code,
};
use serde_json::{Value, json};
use support::{
    assert_accepted_and_closed, assert_clean_send, driver, evidence, finish_scenario, require,
    wire_fake_llm,
};

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

/// 接受、最终根路径正文与树形日志共同证明同一个 Turn。
fn assert_live_text_turn(
    outcome: &SendOutcome,
    session_id: &str,
    prompt: &str,
    reply: &str,
    log: &Value,
) -> Result<(), AssertError> {
    assert_accepted_and_closed(outcome)?;
    let (sid, turn_id, result) =
        outcome
            .closed_result()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "closed result".into(),
            })?;
    require(
        sid == session_id
            && result == "completed"
            && outcome.exit_code == Some(SendExitCode::Committed)
            && derive_send_exit_code(&outcome.events) == SendExitCode::Committed,
        "completed identity and delivery must agree",
    )?;
    let history = outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "history" => {
                Some(&raw["history"])
            }
            _ => None,
        })
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: "complete history".into(),
        })?;
    let entries: Vec<_> = history["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["content"]["turn_id"] == turn_id)
        .collect();
    let text_parts = |text: &str| json!([{"replay_state":null,"semantic":{"text":{"text":text}}}]);
    require(
        entries.len() == 2
            && entries[0]["content"]["message"]["parts"] == text_parts(prompt)
            && entries[1]["content"]["content"]["assistant"]["parts"] == text_parts(reply)
            && entries[1]["content"]["content"]["results"] == json!([])
            && entries
                .iter()
                .all(|entry| entry["summary"] == json!({"outcome":"completed","notice":null})),
        "history contains exact user and assistant text with the Turn outcome",
    )?;
    require(
        log["session"]["session_id"] == session_id
            && log["nodes"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["turn_start"]["turn_id"] == turn_id
                        && row["turn_start"]["summary"]["outcome"] == "completed"
                })
            }),
        "tree log corroborates the same completed Turn",
    )
}

fn request_messages(capture: &CapturedRequest) -> Vec<(&str, &str)> {
    capture.raw["messages"]
        .as_array()
        .expect("provider messages")
        .iter()
        .map(|message| {
            (
                message["role"].as_str().expect("role"),
                message["content"].as_str().expect("text content"),
            )
        })
        .collect()
}

/// 真实文本 Turn：provider 捕获请求，流或固定正文交付回答，退出分类与 session log 互证提交。
#[test]
fn direct_local_plain_text_turn_commits_and_exit_zero() {
    let driver = driver();
    let (world, spec) = turn_world("direct-local-plain-turn", "dl-plain", "[]", "");
    let server = wire_fake_llm(&world, "dl-plain", SseScript::plain("ok"));
    let scenario_evidence = evidence(&world, &spec);

    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-plain");
    let execution = send.send("ping").expect("run session send");
    let outcome = &execution.parsed;
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id().unwrap()])
        .expect("session log --json");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        require(log_output.is_clean_exit(0), "session log must exit cleanly")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("session log JSON");
        assert_live_text_turn(outcome, send.session_id().unwrap(), "ping", "ok", &log)?;
        require(
            log["nodes"].as_array().is_some_and(|rows| rows.len() == 3),
            "root, user and Step only",
        )?;
        require(
            captures.len() == 1,
            "plain Turn must make exactly one real provider request",
        )?;
        let messages = request_messages(&captures[0]);
        require(
            captures[0].model == "dl-plain"
                && messages.len() == 2
                && messages[0].0 == "system"
                && messages[1] == ("user", "ping"),
            "fake LLM must receive model=dl-plain and the exact current user input",
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

/// 同一 Session 两次发送之间发生外部编辑与提交；重开保留项目文件和 Git HEAD，
/// 第二次真实模型请求必须携带第一次的完整对话，两次 live 提交均可由固定结果与日志互证。
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-reopen");

    let first = send.send("ping").expect("first turn drives to completion");
    world
        .write_project_file("external.txt", "external edit between turns\n")
        .expect("external file");
    world.git(&["add", "external.txt"]).expect("git add");
    world
        .git(&["commit", "-m", "external change"])
        .expect("git commit");
    let project_head = || {
        gix::open_opts(world.project_dir(), gix::open::Options::isolated())
            .expect("open project Git")
            .head_id()
            .expect("project Git HEAD")
            .detach()
    };
    let external_commit = project_head();
    let second = send
        .send("again")
        .expect("second turn drives to completion");
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id().unwrap()])
        .expect("session log --json");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(log_output.is_clean_exit(0), "session log must exit cleanly")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("session log JSON");
        assert_live_text_turn(&first, send.session_id().unwrap(), "ping", "pong-one", &log)?;
        assert_live_text_turn(
            &second,
            send.session_id().unwrap(),
            "again",
            "pong-two",
            &log,
        )?;
        require(
            captures.len() == 2,
            format!("expected one LLM request per turn, got: {captures:?}"),
        )?;
        let first_messages = request_messages(&captures[0]);
        let second_messages = request_messages(&captures[1]);
        require(
            captures.iter().all(|capture| capture.model == "dl-reopen")
                && first_messages.len() == 2
                && first_messages[0].0 == "system"
                && first_messages[1] == ("user", "ping")
                && second_messages.len() == 4
                && second_messages[0].0 == "system"
                && second_messages[1..]
                    == [
                        ("user", "ping"),
                        ("assistant", "pong-one"),
                        ("user", "again"),
                    ],
            "two real requests must carry the exact inputs and prior user/assistant context",
        )?;

        let turn_ids: Vec<_> = [&first, &second]
            .iter()
            .map(|outcome| outcome.closed_result().unwrap().1)
            .collect();
        require(turn_ids[0] != turn_ids[1], "two distinct Turns")?;
        let rows = log["nodes"].as_array().unwrap();
        require(
            rows.len() == 5
                && turn_ids
                    .iter()
                    .all(|id| rows.iter().any(|row| row["turn_start"]["turn_id"] == *id)),
            "tree contains root and both complete Turns",
        )?;
        require(
            std::fs::read(world.project_dir().join("external.txt"))
                .ok()
                .as_deref()
                == Some(b"external edit between turns\n")
                && project_head() == external_commit,
            "reopening the Session must not roll back the exact external file or Git commit",
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
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-deny");
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
            outcome.has_completed(),
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
