// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地直连基础轮次场景：产品轮次直接运行在规范化项目工作区上，无 `[workspace]`、`[lancet]`
//! 或 `[tools]` 配置段。
//!
//! - **纯文本轮次** — 完整的纯文本轮次持久提交并退出 0，`session log --json` 与实时输出互证；
//! - **外部变更后重开会话** — 两次 `session send` 之间发生外部文件编辑与 Git 提交，重开会话保留
//!   外部文件与 Git `HEAD`，且第二次模型请求携带完整上文对话。

mod support;

use lattice_system_harness::{
    AssertError, CapturedRequest, ProviderFixture, ScenarioSpec, ScenarioWorld, SendExitCode,
    SendOutcome, SessionSendDriver, SseScript, derive_send_exit_code,
};
use serde_json::{Value, json};
use support::{
    assert_accepted_and_closed, assert_clean_send, driver, evidence, finish_scenario, require,
    wire_fake_llm,
};

/// 构造带手写 `agent.toml` 的本地直连场景世界（harness 黑盒边界禁止依赖 `lattice-profile`）。
fn turn_world(prefix: &str, agent: &str) -> (ScenarioWorld, ScenarioSpec) {
    let spec = ScenarioSpec::direct_local(prefix);
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界");
    let model = ProviderFixture::chat_completions(agent)
        .model_ref()
        .unwrap();
    let toml = format!(
        "schema_version = 1\n\n[agent]\nname = \"{agent}\"\nmodel = \"{model}\"\n\n[prompt]\nsystem = \"You are a concise test agent. Reply briefly.\"\n"
    );
    world
        .write_project_file(&format!(".lattice/agents/{agent}/agent.toml"), &toml)
        .expect("写入 agent.toml");
    world.init_git_project().expect("初始化 Git 项目");
    (world, spec)
}

/// 接受回执、本轮对话条目与树形日志共同证明同一轮次；`ordinal` 是从根数起的轮次序号。
fn assert_live_text_turn(
    outcome: &SendOutcome,
    session_id: &str,
    prompt: &str,
    reply: &str,
    ordinal: u64,
    log: &Value,
) -> Result<(), AssertError> {
    assert_accepted_and_closed(outcome)?;
    let (sid, turn_id, result) =
        outcome
            .closed_result()
            .ok_or_else(|| AssertError::StdoutMissing {
                expected: "轮次收束结果".into(),
            })?;
    require(
        sid == session_id
            && result == "completed"
            && outcome.exit_code == Some(SendExitCode::Committed)
            && derive_send_exit_code(&outcome.records) == SendExitCode::Committed,
        "已完成轮次的身份标识与退出码分类必须一致",
    )?;
    let entries = outcome.entries();
    let text_parts = |text: &str| json!([{"replay_state":null,"semantic":{"text":{"text":text}}}]);
    require(
        entries.len() == 2
            && entries.iter().all(|entry| entry.turn_id == turn_id)
            && entries[0].user_ordinal() == Some(ordinal)
            && entries[0].raw["content"]["message"]["parts"] == text_parts(prompt)
            && entries[1].raw["content"]["reply"]["parts"] == text_parts(reply)
            && entries[1].raw["content"]["slots"] == json!([])
            && entries[1].raw["summary"]
                == json!({"outcome":"completed","notice":null,"progress":{"progress":"closed","end":{"end":"finish"}}}),
        "本轮条目必须携带一致的用户输入、助手回复、轮次序号与完成结局",
    )?;
    require(
        log["session"]["session_id"] == session_id
            && log["nodes"].as_array().is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["turn_start"]["turn_id"] == turn_id
                        && row["turn_start"]["summary"]["outcome"] == "completed"
                })
            }),
        "会话树日志必须能互证同一已完成轮次",
    )
}

fn request_messages(capture: &CapturedRequest) -> Vec<(&str, &str)> {
    capture.raw["messages"]
        .as_array()
        .expect("provider 请求应包含 messages 数组")
        .iter()
        .map(|message| {
            (
                message["role"].as_str().expect("消息角色字段"),
                message["content"].as_str().expect("消息文本内容字段"),
            )
        })
        .collect()
}

/// 真实文本轮次：provider 捕获请求，流或固定正文交付回答，退出分类与 `session log` 互证提交。
#[test]
fn direct_local_plain_text_turn_commits_and_exit_zero() {
    let driver = driver();
    let (world, spec) = turn_world("direct-local-plain-turn", "dl-plain");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(&world, "dl-plain", SseScript::plain("ok"));
    let scenario_evidence = evidence(&world, &spec);

    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-plain");
    let execution = send.send("ping").expect("执行会话发送");
    let outcome = &execution.parsed;
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id().unwrap()])
        .expect("查询 JSON 会话日志");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&execution)?;
        require(log_output.is_clean_exit(0), "会话日志命令必须正常退出")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("解析会话日志 JSON");
        assert_live_text_turn(outcome, send.session_id().unwrap(), "ping", "ok", 1, &log)?;
        require(
            log["nodes"].as_array().is_some_and(|rows| rows.len() == 3),
            "会话树应仅含根节点、用户节点与步骤节点",
        )?;
        require(
            captures.len() == 1,
            "纯文本单轮对话必须恰好发起一次真实 provider 请求",
        )?;
        let messages = request_messages(&captures[0]);
        require(
            captures[0].model == "dl-plain"
                && messages.len() == 2
                && messages[0].0 == "system"
                && messages[1] == ("user", "ping"),
            "假 provider 必须收到目标模型 dl-plain 与原样当前用户输入",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("单轮纯文本会话发送", &execution.command),
            ("查询会话日志", &log_output),
        ],
        captures,
        result,
    );
}

/// 同一会话两次发送之间发生外部编辑与提交；重开保留项目文件和 Git `HEAD`，
/// 第二次真实模型请求必须携带第一次的完整对话，两次实时提交均可由固定结果与日志互证。
#[test]
fn direct_local_session_reopen_after_external_change() {
    let driver = driver();
    let (world, spec) = turn_world("direct-local-reopen", "dl-reopen");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let server = wire_fake_llm(
        &world,
        "dl-reopen",
        SseScript::Sequence {
            scripts: vec![SseScript::plain("pong-one"), SseScript::plain("pong-two")],
        },
    );
    let scenario_evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "dl-reopen");

    let first = send.send("ping").expect("第一轮对话驱动至完成");
    world
        .write_project_file("external.txt", "external edit between turns\n")
        .expect("写入外部变更文件");
    world
        .git(&["add", "external.txt"])
        .expect("暂存外部变更文件");
    world
        .git(&["commit", "-m", "external change"])
        .expect("提交外部变更");
    let project_head = || {
        gix::open_opts(world.project_dir(), gix::open::Options::isolated())
            .expect("打开项目 Git 仓库")
            .head_id()
            .expect("读取项目 Git HEAD")
            .detach()
    };
    let external_commit = project_head();
    let second = send.send("again").expect("第二轮对话驱动至完成");
    let log_output = send
        .run_command(&["--json", "session", "log", send.session_id().unwrap()])
        .expect("查询 JSON 会话日志");
    let captures = server.captured_requests();

    let result: Result<(), AssertError> = (|| {
        assert_clean_send(&first)?;
        assert_clean_send(&second)?;
        require(log_output.is_clean_exit(0), "会话日志命令必须正常退出")?;
        let log: Value = serde_json::from_slice(&log_output.stdout).expect("解析会话日志 JSON");
        assert_live_text_turn(
            &first,
            send.session_id().unwrap(),
            "ping",
            "pong-one",
            1,
            &log,
        )?;
        assert_live_text_turn(
            &second,
            send.session_id().unwrap(),
            "again",
            "pong-two",
            2,
            &log,
        )?;
        require(
            captures.len() == 2,
            format!("每轮对话应各发起一次模型请求，实际捕获：{captures:?}"),
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
            "两次真实请求必须携带原样输入以及上一轮用户与助手上下文",
        )?;

        let turn_ids: Vec<_> = [&first, &second]
            .iter()
            .map(|outcome| outcome.closed_result().unwrap().1)
            .collect();
        require(turn_ids[0] != turn_ids[1], "两轮对话应具有不同的轮次标识")?;
        let rows = log["nodes"].as_array().unwrap();
        require(
            rows.len() == 5
                && turn_ids
                    .iter()
                    .all(|id| rows.iter().any(|row| row["turn_start"]["turn_id"] == *id)),
            "会话树应包含根节点与全部两轮已完成对话",
        )?;
        require(
            std::fs::read(world.project_dir().join("external.txt"))
                .ok()
                .as_deref()
                == Some(b"external edit between turns\n")
                && project_head() == external_commit,
            "重开会话绝不能回滚外部文件内容或 Git 提交",
        )?;
        Ok(())
    })();

    finish_scenario(
        scenario_evidence,
        &[
            ("第一轮会话发送", &first.command),
            ("第二轮会话发送", &second.command),
            ("查询会话日志", &log_output),
        ],
        captures,
        result,
    );
}
