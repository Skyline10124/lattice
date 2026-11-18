// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 用户图片输入在真实前后台二进制下的五协议投影、跨重启重放、本地预算与远端 413 场景。

mod support;

use lattice_system_harness::{
    AssertError, CommandOutcome, FAKE_API_KEY, FakeLlmServer, ProviderFixture, ProviderProtocol,
    ProviderTarget, ScenarioSpec, ScenarioWorld, SendExecution, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};
use support::{
    assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence, finish_recorded,
    live_turn, parse_send_stdout, record_session, rejected_kind, require, sequence, wire_fake_llm,
};

const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_B64: &str = "iVBORw0KGgo=";
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
const JPEG_B64: &str = "/9j/4A==";
const BIG_IMAGE_BYTES: usize = 3 * 1024 * 1024;

const BUDGET_EXCEEDED_NOTICE: &str = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片、执行 lattice session rewind 退回较早节点或新建会话";
const HTTP_413_NOTICE: &str = "目标服务拒绝了本次请求大小（HTTP 413），请减少图片、执行 lattice session rewind 退回较早节点或新建会话";
const REMOTE_413_MESSAGE: &str = "remote request payload exceeds gateway limit";

fn execution(command: CommandOutcome) -> SendExecution {
    SendExecution {
        parsed: parse_send_stdout(&command.stdout, command.exit_code()),
        command,
    }
}

/// 输出合法、本轮被接受并关闭的一次发送。
fn accepted(command: CommandOutcome) -> Result<SendExecution, AssertError> {
    let execution = execution(command);
    assert_clean_send(&execution)?;
    assert_accepted_and_closed(&execution)?;
    Ok(execution)
}

/// 本轮用户消息条目的片段（含结构化图片引用）。
fn user_message_parts(execution: &SendExecution) -> Result<Vec<Value>, AssertError> {
    let entry = execution
        .entries()
        .into_iter()
        .find(|entry| entry.user_ordinal().is_some())
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: "逐行输出中缺少本轮用户消息条目".into(),
        })?;
    entry.raw["content"]["message"]["parts"]
        .as_array()
        .cloned()
        .ok_or_else(|| AssertError::StdoutMissing {
            expected: "用户消息条目的片段列表必须为数组".into(),
        })
}

fn assert_image_part(part: &Value, media_type: &str, byte_length: u64) -> Result<(), AssertError> {
    let image = &part["semantic"]["image"]["image"];
    let sha256 = image["sha256"].as_str().unwrap_or_default();
    require(
        image["media_type"] == media_type
            && image["byte_length"] == byte_length
            && sha256.len() == 64
            && sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        format!("图片片段应为 {media_type}、{byte_length} 字节且带 64 位十六进制摘要：{part}"),
    )
}

/// 五种协议里，本轮“文本 + PNG + JPEG”用户消息的内容块（见 `docs/architecture/llm/protocols.md`）。
fn expected_user_blocks(protocol: ProviderProtocol, text: &str) -> [Value; 3] {
    match protocol {
        ProviderProtocol::ChatCompletions => [
            json!({"type": "text", "text": text}),
            json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{PNG_B64}")}}),
            json!({"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{JPEG_B64}")}}),
        ],
        ProviderProtocol::Responses => [
            json!({"type": "input_text", "text": text}),
            json!({"type": "input_image", "image_url": format!("data:image/png;base64,{PNG_B64}"), "detail": "auto"}),
            json!({"type": "input_image", "image_url": format!("data:image/jpeg;base64,{JPEG_B64}"), "detail": "auto"}),
        ],
        ProviderProtocol::AnthropicMessages => [
            json!({"type": "text", "text": text}),
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": PNG_B64}}),
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": JPEG_B64}}),
        ],
        ProviderProtocol::GenerateContent => [
            json!({"text": text}),
            json!({"inlineData": {"mimeType": "image/png", "data": PNG_B64}}),
            json!({"inlineData": {"mimeType": "image/jpeg", "data": JPEG_B64}}),
        ],
        ProviderProtocol::Interactions => [
            json!({"type": "text", "text": text}),
            json!({"type": "image", "mime_type": "image/png", "data": PNG_B64}),
            json!({"type": "image", "mime_type": "image/jpeg", "data": JPEG_B64}),
        ],
    }
}

/// 请求中的对话消息（Chat Completions 去掉开头的系统提示）。
fn conversation(protocol: ProviderProtocol, raw: &Value) -> Vec<Value> {
    let (key, skip) = match protocol {
        ProviderProtocol::ChatCompletions => ("messages", 1),
        ProviderProtocol::AnthropicMessages => ("messages", 0),
        ProviderProtocol::Responses | ProviderProtocol::Interactions => ("input", 0),
        ProviderProtocol::GenerateContent => ("contents", 0),
    };
    raw[key]
        .as_array()
        .map(|messages| messages.iter().skip(skip).cloned().collect())
        .unwrap_or_default()
}

fn content_blocks(protocol: ProviderProtocol, message: &Value) -> &Value {
    match protocol {
        ProviderProtocol::GenerateContent => &message["parts"],
        _ => &message["content"],
    }
}

fn session_count(world: &ScenarioWorld, spec: &ScenarioSpec) -> usize {
    let out = support::cli(world, spec, &["--json", "session", "list"]);
    assert!(out.is_clean_exit(0), "查询会话列表应退出 0");
    let rows: Value = serde_json::from_slice(&out.stdout).expect("解析会话列表输出失败");
    rows.as_array().expect("会话列表应为 JSON 数组").len()
}

fn body_len(raw: &Value) -> usize {
    serde_json::to_vec(raw).expect("序列化模型请求体失败").len()
}

#[test]
fn user_images_replay_after_daemon_restart() {
    let protocols = [
        ("chat-completions", ProviderProtocol::ChatCompletions),
        ("responses", ProviderProtocol::Responses),
        ("interactions", ProviderProtocol::Interactions),
        ("anthropic-messages", ProviderProtocol::AnthropicMessages),
        ("generate-content", ProviderProtocol::GenerateContent),
    ];
    let driver = driver();
    for (label, protocol) in protocols {
        let spec = ScenarioSpec::direct_local(format!("user-images-{label}"));
        let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
        let provider_id = format!("img-{label}");
        let fixture = ProviderFixture {
            protocol,
            target: ProviderTarget::Custom(&provider_id),
            model: "vision-model",
        };
        let agent = format!("agent-{label}");
        world
            .write_agent_profile(&agent, &fixture.model_ref().expect("生成模型引用失败"))
            .expect("写入 agent.toml 失败");
        world.init_git_project().expect("初始化 Git 项目失败");
        world.budget(&spec);
        world.start_daemon().expect("显式启动后台");
        let mut server = FakeLlmServer::start(sequence(vec![
            SseScript::plain("第一轮回复"),
            SseScript::plain("第二轮回复"),
        ]))
        .expect("启动假模型服务失败");
        world
            .wire_provider(fixture, &server, FAKE_API_KEY)
            .expect("接入假 provider 配置失败");
        // 扩展名故意与内容不符：格式只按文件头魔数识别。
        let shot = world.project_dir().join("shot.jpg");
        let photo = world.project_dir().join("photo.jpeg");
        std::fs::write(&shot, PNG_MAGIC).expect("写入 PNG 测试文件失败");
        std::fs::write(&photo, JPEG_MAGIC).expect("写入 JPEG 测试文件失败");

        let mut evidence = evidence(&world, &spec);
        let send = SessionSendDriver::new(&driver, &world, &spec, &agent);
        let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let first = send
                .run_command(&[
                    "--json",
                    "session",
                    "send",
                    "请比较这两张图",
                    "--agent",
                    &agent,
                    "--image",
                    "shot.jpg",
                    "--image",
                    "photo.jpeg",
                ])
                .expect("执行首轮含图发送失败");
            evidence.record_command(format!("{label} 首轮含图发送"), &first);
            let first = accepted(first).unwrap();
            require(
                first.is_clean_exit(0) && first.has_completed(),
                "首轮含图发送应退出 0 并完成",
            )
            .unwrap();
            let sid = first.accepted_session_id().unwrap().to_owned();
            let parts = user_message_parts(&first).unwrap();
            require(
                parts.len() == 3 && parts[0]["semantic"]["text"]["text"] == "请比较这两张图",
                format!("首轮用户消息应为文本加两张图片：{parts:?}"),
            )
            .unwrap();
            assert_image_part(&parts[1], "png", 8).unwrap();
            assert_image_part(&parts[2], "jpeg", 4).unwrap();

            // 删掉图片文件再重启后台：续接只能从会话库重放图片。
            std::fs::remove_file(&shot).expect("删除 PNG 测试文件失败");
            std::fs::remove_file(&photo).expect("删除 JPEG 测试文件失败");
            world.restart_daemon().expect("重启后台失败");
            let second = send
                .run_command(&["--json", "session", "send", "继续", "--session", &sid])
                .expect("执行重启后续接失败");
            evidence.record_command(format!("{label} 重启后续接"), &second);
            let second = accepted(second).unwrap();
            require(
                second.is_clean_exit(0) && second.reply_text() == "第二轮回复",
                "重启后续接应退出 0 并完成",
            )
            .unwrap();

            let captures = server.captured_requests();
            require(captures.len() == 2, "两轮应恰好产生 2 次模型请求").unwrap();
            let first_request = conversation(protocol, &captures[0].raw);
            require(
                first_request.len() == 1,
                format!("{label} 第一次请求应只含首轮用户消息"),
            )
            .unwrap();
            let expected = expected_user_blocks(protocol, "请比较这两张图");
            require(
                *content_blocks(protocol, &first_request[0]) == json!(expected),
                format!(
                    "{label} 首轮用户消息的内容块应与协议合同一致：{}",
                    content_blocks(protocol, &first_request[0])
                ),
            )
            .unwrap();
            let second_request = conversation(protocol, &captures[1].raw);
            require(
                second_request.len() == 3
                    && second_request[0] == first_request[0]
                    && second_request[1].to_string().contains("第一轮回复")
                    && second_request[2].to_string().contains("继续"),
                format!("{label} 重启后的请求应按原顺序重放首轮用户消息、首轮回复与本轮文本"),
            )
            .unwrap();

            evidence.record_captures(captures);
            server
                .shutdown(spec.step_timeout())
                .expect("关闭假模型服务失败");
            record_session(
                &send,
                &mut evidence,
                &sid,
                json!({"protocol": label, "turns": 2}),
            );
        }));
        finish_recorded(evidence, checked);
    }
}

#[test]
fn user_image_budget_recovers_after_rewind() {
    let driver = driver();
    let (world, spec) = direct_world("user-image-budget", "vision", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let mut server = wire_fake_llm(&world, "vision", SseScript::plain("好"));
    let mut big = vec![0xAB; BIG_IMAGE_BYTES];
    big[..PNG_MAGIC.len()].copy_from_slice(PNG_MAGIC);
    std::fs::write(world.project_dir().join("big.png"), &big).expect("写入 3 MiB 测试图失败");
    std::fs::write(world.project_dir().join("bad.png"), b"not-an-image")
        .expect("写入非图片文件失败");
    let three_images = [
        "--image", "big.png", "--image", "big.png", "--image", "big.png",
    ];

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "vision");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let run = |label: &str, args: &[&str]| {
            let mut full = vec!["--json", "session", "send"];
            full.extend_from_slice(args);
            full.extend_from_slice(&three_images);
            send.run_command(&full)
                .unwrap_or_else(|error| panic!("执行{label}失败：{error}"))
        };

        // 每轮 3 张 3 MiB（9 MiB 原始字节，IPC 帧约 12 MiB），前两轮在 32 MiB 预算内。
        let first = run("第一轮", &["第一轮", "--agent", "vision"]);
        evidence.record_command("第一轮发送三张 3 MiB 图片", &first);
        let first = accepted(first).unwrap();
        require(
            first.is_clean_exit(0) && first.has_completed(),
            "第一轮应退出 0 并完成",
        )
        .unwrap();
        let sid = first.accepted_session_id().unwrap().to_owned();
        let second = run("第二轮", &["第二轮", "--session", &sid]);
        evidence.record_command("第二轮续接三张 3 MiB 图片", &second);
        let second = accepted(second).unwrap();
        require(
            second.is_clean_exit(0) && second.has_completed(),
            "第二轮应退出 0 并完成",
        )
        .unwrap();
        let lengths: Vec<usize> = server
            .captured_requests()
            .iter()
            .map(|capture| body_len(&capture.raw))
            .collect();
        require(
            lengths.len() == 2 && lengths[1] > lengths[0] * 19 / 10,
            format!("前两轮应各有一次请求，第二次正文约为第一次的两倍：{lengths:?}"),
        )
        .unwrap();

        // 本地预检拒绝：不发出 configure，不留草稿会话，也不请求模型。
        let before = session_count(&world, &spec);
        let bad = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "非图片",
                "--agent",
                "vision",
                "--image",
                "bad.png",
            ])
            .expect("执行非图片发送失败");
        evidence.record_command("本地预检拒绝非图片文件", &bad);
        let bad = execution(bad);
        require(
            bad.is_clean_exit(2)
                && bad.configures().count() == 0
                && rejected_kind(&bad) == Some("bad_request"),
            "非图片文件应在本地以 bad_request 拒绝并退出 2，且不发出 configure",
        )
        .unwrap();
        require(
            session_count(&world, &spec) == before && server.captured_requests().len() == 2,
            "本地预检拒绝后会话数与模型请求数都不变",
        )
        .unwrap();

        // 第三轮累计约 36 MiB，在接受前按本地预算拒绝。
        let third = run("第三轮", &["第三轮", "--session", &sid]);
        evidence.record_command("第三轮累计超过 32 MiB 被拒", &third);
        let third = execution(third);
        let rejection = third.rejection().expect("超预算应给出 rejected 回执");
        require(
            third.is_clean_exit(2)
                && rejection.kind == "bad_request"
                && rejection.message == BUDGET_EXCEEDED_NOTICE,
            format!("超预算应以 bad_request 与固定说明拒绝并退出 2：{rejection:?}"),
        )
        .unwrap();
        require(
            server.captured_requests().len() == 2,
            "接受前被拒的一轮不请求模型",
        )
        .unwrap();

        // 退回根节点后再发同样三张图，只含本轮图片，回到预算内。
        let rewind = send
            .run_command(&["--json", "session", "rewind", &sid, "0"])
            .expect("执行 rewind 失败");
        evidence.record_command("退回根节点 0", &rewind);
        require(rewind.is_clean_exit(0), "rewind 到根节点应退出 0").unwrap();
        let fourth = run("退回后重发", &["退回后重发", "--session", &sid]);
        evidence.record_command("退回后重新发送三张 3 MiB 图片", &fourth);
        let fourth = accepted(fourth).unwrap();
        require(
            fourth.is_clean_exit(0) && fourth.has_completed(),
            "退回后重发应退出 0 并完成",
        )
        .unwrap();
        let captures = server.captured_requests();
        let third_len = body_len(&captures[2].raw);
        require(
            captures.len() == 3
                && captures[2].message_roles == ["system", "user"]
                && third_len.abs_diff(lengths[0]) <= 1024,
            format!("退回后的请求只含本轮用户消息，正文约 12 MiB：{third_len}"),
        )
        .unwrap();
        drop(captures);

        // 大请求不整份留证，只记请求数与正文长度。
        let summary =
            json!({"request_count": 3, "body_lengths": [lengths[0], lengths[1], third_len]});
        evidence
            .record_json("request-lengths", &summary)
            .expect("记录请求长度失败");
        server
            .shutdown(spec.step_timeout())
            .expect("关闭假模型服务失败");
        record_session(&send, &mut evidence, &sid, summary);
    }));
    finish_recorded(evidence, checked);
}

#[test]
fn user_image_http_413_is_not_retried() {
    let driver = driver();
    let (world, spec) = direct_world(
        "user-image-413",
        "vision",
        "[llm.retry]\nmax_retries = 2\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let mut server = wire_fake_llm(
        &world,
        "vision",
        sequence(vec![
            SseScript::HttpResponse {
                status: 413,
                content_type: "application/json".into(),
                headers: vec![("x-should-retry".into(), "true".into())],
                body: json!({"error": {"message": REMOTE_413_MESSAGE}}).to_string(),
            },
            SseScript::plain("不应出现"),
        ]),
    );
    std::fs::write(world.project_dir().join("small.png"), PNG_MAGIC).expect("写入小图失败");

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "vision");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let command = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "触发远端 413",
                "--agent",
                "vision",
                "--image",
                "small.png",
            ])
            .expect("执行含图发送失败");
        evidence.record_command("触发远端 HTTP 413 的含图发送", &command);
        let outcome = accepted(command).unwrap();
        let turn = live_turn(&outcome);
        let end = &turn["steps"].as_array().unwrap().last().unwrap()["end"];
        require(
            outcome.is_clean_exit(1)
                && turn["summary"]["outcome"] == "failed"
                && end["end"] == "stop"
                && end["cause"]["cause"] == "provider_error",
            format!("远端 413 应以 provider_error 结束本轮并退出 1：{end}"),
        )
        .unwrap();
        require(
            server.captured_requests().len() == 1,
            "打开重试时 413 仍只请求一次",
        )
        .unwrap();
        let notice = turn["summary"]["notice"]["text"]
            .as_str()
            .unwrap_or_default();
        require(
            notice.contains(HTTP_413_NOTICE) && notice.contains(REMOTE_413_MESSAGE),
            format!("结局提示应含 413 恢复说明与远端摘要：{notice}"),
        )
        .unwrap();
        evidence.record_captures(server.captured_requests());
        server
            .shutdown(spec.step_timeout())
            .expect("关闭假模型服务失败");
        let sid = outcome.accepted_session_id().unwrap().to_owned();
        record_session(&send, &mut evidence, &sid, json!({"status": 413}));
    }));
    finish_recorded(evidence, checked);
}
