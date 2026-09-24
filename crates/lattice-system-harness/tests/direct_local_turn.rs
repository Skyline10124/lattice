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
    SessionSendDriver, SseScript, TurnEventLine, assert_event_subsequence, derive_send_exit_code,
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

/// 接受、固定正文与公开日志共同证明 live Turn；有 Gap 时不伪造缺失事件。
fn assert_live_text_turn(
    outcome: &SendOutcome,
    session_id: &str,
    prompt: &str,
    reply: &str,
    log: &Value,
) -> Result<(), AssertError> {
    assert_accepted_and_closed(outcome)?;
    let (committed_session, commit_id, result) =
        outcome
            .committed_result()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "strict committed result".into(),
            })?;
    require(
        committed_session == session_id
            && !commit_id.is_empty()
            && result == "completed"
            && outcome.exit_code == Some(SendExitCode::Committed)
            && derive_send_exit_code(&outcome.events) == SendExitCode::Committed,
        "completed result identity and both exit classifications must agree",
    )?;
    let has_gap = outcome.events.iter().any(
        |event| matches!(event, TurnEventLine::Unknown { type_name, .. } if type_name == "gap"),
    );
    if !has_gap {
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
        require(
            outcome.assistant_text() == reply,
            "continuous stream must carry the exact reply",
        )?;
    }
    let turn_id = outcome.accepted_turn_id().expect("accepted TurnId");
    require(
        outcome.events.iter().any(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => {
                let view = &raw["view"];
                view["session"]["session_id"] == session_id
                    && view["active_turn"].is_null()
                    && view["requested_turn"]["turn_id"] == turn_id
                    && view["requested_turn"]["commit_id"] == commit_id
                    && view["requested_turn"]["origin"] == "live"
            }
            _ => false,
        }),
        "fixed result must identify this live Turn and commit",
    )?;
    // 无论是否发生 Gap，都按最终 commit 固定正文核验；pending 正文不能代替提交内容。
    let transcript: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "transcript"
                    && raw["state"] == "completed"
                    && raw["object_id"] == commit_id =>
            {
                Some((&raw["message"]["role"], &raw["message"]["parts"]))
            }
            _ => None,
        })
        .collect();
    let text_parts =
        |text: &str| json!([{"replay_state": null, "semantic": {"text": {"text": text}}}]);
    require(
        transcript
            == vec![
                (&json!("user"), &text_parts(prompt)),
                (&json!("assistant"), &text_parts(reply)),
            ],
        "fixed committed transcript must contain exactly the user input and assistant reply",
    )?;
    require(
        log["session_id"] == session_id
            && log["turns"].as_array().is_some_and(|turns| {
                turns.iter().any(|turn| {
                    turn["turn_id"] == turn_id
                        && turn["commit_id"] == commit_id
                        && turn["commit_message"] == prompt
                        && turn["origin"] == "live"
                        && turn["outcome"] == json!({"completed": {"finish_reason": "stop"}})
                        && turn["message_count"] == 2
                        && turn["tool_message_count"] == 0
                })
            }),
        "session log must corroborate the same live completed Turn and commit",
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
        require(log_output.is_clean_exit(0), "session log must exit cleanly")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("session log JSON");
        assert_live_text_turn(outcome, send.session_id(), "ping", "ok", &log)?;
        require(
            log["turns"]
                .as_array()
                .is_some_and(|turns| turns.len() == 1),
            "one committed Turn in log",
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
        .run_command(&["--json", "session", "log", send.session_id()])
        .expect("session log --json");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(log_output.is_clean_exit(0), "session log must exit cleanly")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("session log JSON");
        assert_live_text_turn(&first, send.session_id(), "ping", "pong-one", &log)?;
        assert_live_text_turn(&second, send.session_id(), "again", "pong-two", &log)?;
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

        let commit_ids: Vec<&str> = [&first, &second]
            .iter()
            .filter_map(|outcome| outcome.committed_result().map(|(_, id, _)| id))
            .collect();
        require(commit_ids.len() == 2, "both turns must report commit ids")?;
        require(
            commit_ids[0] != commit_ids[1],
            "two Turns must have distinct commit ids",
        )?;
        require(
            log["turns"].as_array().is_some_and(|turns| {
                turns.len() == 2
                    && commit_ids
                        .iter()
                        .all(|id| turns.iter().any(|turn| turn["commit_id"] == *id))
            }),
            "session log must contain exactly the two reported commits",
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
