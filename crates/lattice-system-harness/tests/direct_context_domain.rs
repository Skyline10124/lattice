// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! ADR 0014 模型上下文域公开 CLI 与端到端行为场景：
//! - 多渠道 `session feed`、`session inspect`、`session context-op`、`session trigger`；
//! - 全部 7 种 `ContextEdit`（含 Anthropic 签名推理剥离、无 `rewind` 图片恢复、多工具调用编辑）；
//! - `send`/`feed`/`context-op`/`trigger` 的 `--expected-position` 并发冲突保护。

mod support;

use lattice_system_harness::{
    AssertError, CommandOutcome, FAKE_API_KEY, FakeLlmServer, ProviderFixture, ProviderProtocol,
    ProviderTarget, ScenarioSpec, ScenarioWorld, SendExecution, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};
use support::{
    assert_accepted_and_closed, assert_clean_send, direct_world, driver, evidence, finish_recorded,
    parse_send_stdout, record_session, require, sequence, wire_fake_llm,
};

const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const PNG_B64: &str = "iVBORw0KGgo=";
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
const JPEG_B64: &str = "/9j/4A==";
const BIG_IMAGE_BYTES: usize = 3 * 1024 * 1024;
const BUDGET_EXCEEDED_NOTICE: &str = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片，或执行 lattice session context-op 精简历史内容与图片、lattice session rewind 退回较早节点或新建会话";

fn execution(command: CommandOutcome) -> SendExecution {
    SendExecution {
        parsed: parse_send_stdout(&command.stdout, command.exit_code()),
        command,
    }
}

fn accepted(command: CommandOutcome) -> Result<SendExecution, AssertError> {
    let execution = execution(command);
    assert_clean_send(&execution)?;
    assert_accepted_and_closed(&execution)?;
    Ok(execution)
}

fn body_len(raw: &Value) -> usize {
    serde_json::to_vec(raw).expect("序列化模型请求体失败").len()
}

fn image_spec(image: &Value) -> String {
    format!(
        "{}:{}:{}",
        image["media_type"].as_str().expect("图片缺少 media_type"),
        image["byte_length"].as_u64().expect("图片缺少 byte_length"),
        image["sha256"].as_str().expect("图片缺少 sha256"),
    )
}

#[test]
fn context_domain_feed_inspect_edits_trigger_and_expected_position() {
    let driver = driver();
    let (world, spec) = direct_world("context-domain-core", "ctx", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let mut server = wire_fake_llm(
        &world,
        "ctx",
        sequence(vec![
            SseScript::plain("首轮助手回复"),
            SseScript::plain("首次触发回复"),
            SseScript::plain("带期望位点触发回复"),
        ]),
    );
    std::fs::write(world.project_dir().join("shot.png"), PNG_MAGIC).expect("写入 PNG 测试图失败");
    std::fs::write(world.project_dir().join("photo.jpg"), JPEG_MAGIC)
        .expect("写入 JPEG 测试图失败");

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "ctx");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 1. 首轮带自定义通道与图片发送。
        let first_cmd = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "首轮种子消息",
                "--agent",
                "ctx",
                "--channel",
                "cli/seed",
                "--image",
                "shot.png",
            ])
            .expect("执行首轮发送失败");
        evidence.record_command("首轮带 --channel 与 --image 发送", &first_cmd);
        let first = accepted(first_cmd).unwrap();
        require(
            first.is_clean_exit(0) && first.reply_text() == "首轮助手回复",
            "首轮发送应退出 0 并返回首轮助手回复",
        )
        .unwrap();
        let sid = first.accepted_session_id().unwrap().to_owned();

        // 2. 通过 session feed 追加两条多渠道输入（图文与纯文本），均不触发模型请求。
        let feed_note_cmd = send
            .run_command(&[
                "--json",
                "session",
                "feed",
                &sid,
                "待改写笔记",
                "--channel",
                "cli/note",
                "--image",
                "photo.jpg",
            ])
            .expect("执行图文 feed 失败");
        evidence.record_command("feed 追加 cli/note 图文节点", &feed_note_cmd);
        require(feed_note_cmd.is_clean_exit(0), "图文 feed 应退出 0").unwrap();
        let feed_note: Value = serde_json::from_slice(&feed_note_cmd.stdout).unwrap();
        let note_node = feed_note["node"].as_u64().expect("feed 应返回 node");

        let feed_extra_cmd = send
            .run_command(&[
                "session",
                "feed",
                &sid,
                "将被移出的草稿",
                "--channel",
                "cli/extra",
            ])
            .expect("执行纯文本 feed 失败");
        evidence.record_command("feed 追加 cli/extra 文本节点", &feed_extra_cmd);
        require(feed_extra_cmd.is_clean_exit(0), "纯文本 feed 应退出 0").unwrap();
        let extra_stdout = String::from_utf8_lossy(&feed_extra_cmd.stdout);
        let extra_node: u64 = extra_stdout
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("node="))
            .expect("文本模式 feed 应输出 node=<N>")
            .parse()
            .expect("node 应为无符号整数");
        require(
            extra_stdout.contains(&format!("session={sid}")),
            format!("文本模式 feed 应包含 session={sid}：{extra_stdout}"),
        )
        .unwrap();
        require(
            server.captured_requests().len() == 1,
            "session feed 不得触发模型请求",
        )
        .unwrap();

        // 3. session inspect 文本与 JSON 核验有效条目、渠道计数与可复制图片引用。
        let inspect_json_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("执行 JSON inspect 失败");
        evidence.record_command("context-op 前 JSON inspect", &inspect_json_cmd);
        require(inspect_json_cmd.is_clean_exit(0), "JSON inspect 应退出 0").unwrap();
        let inspect_before: Value = serde_json::from_slice(&inspect_json_cmd.stdout).unwrap();
        require(
            inspect_before["position"] == extra_node
                && inspect_before["inspection"]["channel_counts"]["cli/seed"] == 1
                && inspect_before["inspection"]["channel_counts"]["cli/note"] == 1
                && inspect_before["inspection"]["channel_counts"]["cli/extra"] == 1
                && inspect_before["inspection"]["images"]
                    .as_array()
                    .unwrap()
                    .len()
                    == 2,
            format!("inspect JSON 统计与预期不符：{inspect_before}"),
        )
        .unwrap();
        let step1_node = inspect_before["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["kind"] == "step")
            .expect("inspect 应包含首轮 step 条目")["node"]
            .as_u64()
            .unwrap();
        let expected_png_spec = image_spec(&inspect_before["inspection"]["images"][0]);
        let expected_jpg_spec = image_spec(&inspect_before["inspection"]["images"][1]);

        let inspect_text_cmd = send
            .run_command(&["session", "inspect", &sid])
            .expect("执行文本 inspect 失败");
        evidence.record_command("context-op 前文本 inspect", &inspect_text_cmd);
        require(inspect_text_cmd.is_clean_exit(0), "文本 inspect 应退出 0").unwrap();
        let inspect_text = String::from_utf8_lossy(&inspect_text_cmd.stdout);
        require(
            inspect_text.contains(&sid)
                && inspect_text.contains(&format!("@{extra_node}"))
                && inspect_text.contains("cli/seed=1")
                && inspect_text.contains("cli/note=1")
                && inspect_text.contains("cli/extra=1")
                && inspect_text.contains(&expected_png_spec)
                && inspect_text.contains(&expected_jpg_spec),
            format!("文本 inspect 缺少关键字段或可复制图片规格：{inspect_text}"),
        )
        .unwrap();

        // 4. 单条 context-op 组合 --omit-node、--replace-input（带 --image）与 --replace-step。
        let extra_arg = extra_node.to_string();
        let note_arg = note_node.to_string();
        let step1_arg = step1_node.to_string();
        let op_cmd = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid,
                "--omit-node",
                &extra_arg,
                "--replace-input",
                &note_arg,
                "改写后的笔记内容",
                "--image",
                "shot.png",
                "--replace-step",
                &step1_arg,
                "改写后的首轮回复",
                "--channel",
                "cli/edit",
            ])
            .expect("执行组合 context-op 失败");
        evidence.record_command("组合 context-op 编辑上下文", &op_cmd);
        require(op_cmd.is_clean_exit(0), "组合 context-op 应退出 0").unwrap();
        let op_reply: Value = serde_json::from_slice(&op_cmd.stdout).unwrap();
        let op_node = op_reply["node"].as_u64().expect("context-op 应返回 node");

        let inspect_after_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("编辑后 JSON inspect 失败");
        evidence.record_command("context-op 后 JSON inspect", &inspect_after_cmd);
        require(
            inspect_after_cmd.is_clean_exit(0),
            "编辑后 inspect 应退出 0",
        )
        .unwrap();
        let inspect_after: Value = serde_json::from_slice(&inspect_after_cmd.stdout).unwrap();
        require(
            inspect_after["position"] == op_node
                && inspect_after["inspection"]["channel_counts"]
                    .get("cli/extra")
                    .is_none()
                && inspect_after["inspection"]["operated_nodes"]
                    .as_array()
                    .unwrap()
                    .len()
                    == 3,
            format!("编辑后 inspect 应反映 3 个已操作节点并移出 cli/extra：{inspect_after}"),
        )
        .unwrap();

        // 5. session trigger 基于归约后的多渠道上下文触发模型推理。
        let trigger_cmd = send
            .run_command(&[
                "--json",
                "session",
                "trigger",
                &sid,
                "--channel",
                "cli/trigger",
            ])
            .expect("执行 session trigger 失败");
        evidence.record_command("session trigger 触发归约后推理", &trigger_cmd);
        let triggered = accepted(trigger_cmd).unwrap();
        require(
            triggered.is_clean_exit(0) && triggered.reply_text() == "首次触发回复",
            "session trigger 应退出 0 并返回首次触发回复",
        )
        .unwrap();

        let captures = server.captured_requests();
        require(captures.len() == 2, "此时应有 2 次模型请求").unwrap();
        let second_raw = captures[1].raw.to_string();
        require(
            second_raw.contains("<lattice-context channel=\\\"cli/seed\\\">")
                && second_raw.contains("首轮种子消息")
                && second_raw.contains("改写后的首轮回复")
                && !second_raw.contains("首轮助手回复")
                && second_raw.contains("<lattice-context channel=\\\"cli/note\\\">")
                && second_raw.contains("改写后的笔记内容")
                && !second_raw.contains("待改写笔记")
                && !second_raw.contains("将被移出的草稿")
                && second_raw.contains(PNG_B64)
                && !second_raw.contains(JPEG_B64),
            format!("trigger 发往模型的请求体未准确体现多渠道归约结果：{second_raw}"),
        )
        .unwrap();

        // 6. 四类写命令均校验 --expected-position 过期时的 position_changed 冲突（退出码 2）。
        let stale_send = execution(
            send.run_command(&[
                "--json",
                "session",
                "send",
                "过期位点发送",
                "--session",
                &sid,
                "--expected-position",
                "0",
            ])
            .expect("执行过期位点 send 失败"),
        );
        let send_rej = stale_send
            .rejection()
            .expect("过期位点 send 应返回 rejected");
        require(
            stale_send.is_clean_exit(2)
                && send_rej.kind == "conflict"
                && send_rej.conflict.as_deref() == Some("position_changed"),
            format!("过期位点 send 应报 position_changed：{send_rej:?}"),
        )
        .unwrap();

        let stale_feed = send
            .run_command(&[
                "--json",
                "session",
                "feed",
                &sid,
                "过期位点投喂",
                "--expected-position",
                "0",
            ])
            .expect("执行过期位点 feed 失败");
        let stale_feed_err: Value = serde_json::from_slice(&stale_feed.stdout).unwrap();
        require(
            stale_feed.is_clean_exit(2)
                && stale_feed_err["kind"] == "conflict"
                && stale_feed_err["conflict"] == "position_changed",
            format!("过期位点 feed 应报 position_changed：{stale_feed_err}"),
        )
        .unwrap();

        let stale_op = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid,
                "--omit-node",
                &note_arg,
                "--expected-position",
                "0",
            ])
            .expect("执行过期位点 context-op 失败");
        let stale_op_err: Value = serde_json::from_slice(&stale_op.stdout).unwrap();
        require(
            stale_op.is_clean_exit(2)
                && stale_op_err["kind"] == "conflict"
                && stale_op_err["conflict"] == "position_changed",
            format!("过期位点 context-op 应报 position_changed：{stale_op_err}"),
        )
        .unwrap();

        let stale_trigger = execution(
            send.run_command(&[
                "--json",
                "session",
                "trigger",
                &sid,
                "--expected-position",
                "0",
            ])
            .expect("执行过期位点 trigger 失败"),
        );
        let trig_rej = stale_trigger
            .rejection()
            .expect("过期位点 trigger 应返回 rejected");
        require(
            stale_trigger.is_clean_exit(2)
                && trig_rej.kind == "conflict"
                && trig_rej.conflict.as_deref() == Some("position_changed"),
            format!("过期位点 trigger 应报 position_changed：{trig_rej:?}"),
        )
        .unwrap();
        require(
            server.captured_requests().len() == 2,
            "所有过期位点冲突拒绝均不得触发模型请求",
        )
        .unwrap();

        // 使用最新位点 + --model 调用 trigger（验证 configure_once + expected_settings + expected_position 路径）。
        let cur_inspect_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("查询最新位点失败");
        let cur_pos = serde_json::from_slice::<Value>(&cur_inspect_cmd.stdout).unwrap()["position"]
            .as_u64()
            .unwrap();
        let cur_pos_arg = cur_pos.to_string();
        let matched_trigger_cmd = send
            .run_command(&[
                "--json",
                "session",
                "trigger",
                &sid,
                "--model",
                "local/ctx",
                "--expected-position",
                &cur_pos_arg,
            ])
            .expect("执行匹配位点 trigger 失败");
        evidence.record_command(
            "匹配位点与 --model 的 session trigger",
            &matched_trigger_cmd,
        );
        let matched_trigger = accepted(matched_trigger_cmd).unwrap();
        require(
            matched_trigger.is_clean_exit(0)
                && matched_trigger.reply_text() == "带期望位点触发回复",
            "匹配当前位点的 trigger 应成功完成",
        )
        .unwrap();

        let log_cmd = send
            .run_command(&["session", "log", &sid])
            .expect("查询文本 session log 失败");
        evidence.record_command("文本 session log 展示多类节点", &log_cmd);
        let log_text = String::from_utf8_lossy(&log_cmd.stdout);
        require(
            log_cmd.is_clean_exit(0)
                && log_text.contains("input")
                && log_text.contains("context_op")
                && log_text.contains("step"),
            format!("session log 应展示 input、context_op 与 step 节点：{log_text}"),
        )
        .unwrap();

        evidence.record_captures(server.captured_requests());
        server
            .shutdown(spec.step_timeout())
            .expect("关闭假模型服务失败");
        record_session(&send, &mut evidence, &sid, json!({"requests": 3}));
    }));
    finish_recorded(evidence, checked);
}

#[test]
fn context_op_strip_reasoning_clears_anthropic_signature() {
    let driver = driver();
    let spec = ScenarioSpec::direct_local("context-domain-strip-reasoning");
    let world = ScenarioWorld::new(&spec.name).expect("创建场景世界失败");
    let fixture = ProviderFixture {
        protocol: ProviderProtocol::AnthropicMessages,
        target: ProviderTarget::Custom("anth-ctx"),
        model: "claude-ctx",
    };
    world
        .write_agent_profile("anth", &fixture.model_ref().unwrap())
        .expect("写入 Anthropic agent.toml 失败");
    world.init_git_project().expect("初始化 Git 项目失败");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");

    let signed_thinking_reply = SseScript::NativeEvents {
        events: vec![
            (
                Some("message_start".into()),
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}}),
            ),
            (
                Some("content_block_start".into()),
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "thinking", "thinking": "", "signature": ""}
                }),
            ),
            (
                Some("content_block_delta".into()),
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "thinking_delta", "thinking": "secret-chain-of-thought"}
                }),
            ),
            (
                Some("content_block_delta".into()),
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "signature_delta", "signature": "anth-sig-001"}
                }),
            ),
            (
                Some("content_block_stop".into()),
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                Some("content_block_start".into()),
                json!({
                    "type": "content_block_start",
                    "index": 1,
                    "content_block": {"type": "text", "text": ""}
                }),
            ),
            (
                Some("content_block_delta".into()),
                json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": {"type": "text_delta", "text": "带签名的思考回复"}
                }),
            ),
            (
                Some("content_block_stop".into()),
                json!({"type": "content_block_stop", "index": 1}),
            ),
            (
                Some("message_delta".into()),
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn"},
                    "usage": {"output_tokens": 12}
                }),
            ),
            (Some("message_stop".into()), json!({"type": "message_stop"})),
        ],
    };

    let mut server = FakeLlmServer::start(sequence(vec![
        signed_thinking_reply,
        SseScript::plain("剥离前续接回复"),
        SseScript::plain("剥离后触发回复"),
    ]))
    .expect("启动 Anthropic 假模型服务失败");
    world
        .wire_provider(fixture, &server, FAKE_API_KEY)
        .expect("接入 Anthropic 假 provider 失败");

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "anth");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first_cmd = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "请带签名推理",
                "--agent",
                "anth",
            ])
            .expect("执行首轮 Anthropic 发送失败");
        evidence.record_command("首轮 Anthropic 签名推理发送", &first_cmd);
        let first = accepted(first_cmd).unwrap();
        require(
            first.is_clean_exit(0) && first.reply_text() == "带签名的思考回复",
            "首轮应返回带签名的思考回复",
        )
        .unwrap();
        let sid = first.accepted_session_id().unwrap().to_owned();

        // 对照轮：未剥离前续接应原样回放 thinking 与 signature。
        let second_cmd = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "核验剥离前回放",
                "--session",
                &sid,
            ])
            .expect("执行对照续接失败");
        evidence.record_command("剥离前续接核验回放签名", &second_cmd);
        let second = accepted(second_cmd).unwrap();
        require(second.is_clean_exit(0), "对照续接应退出 0").unwrap();
        let before_wire = server.captured_requests()[1].raw.to_string();
        require(
            before_wire.contains("anth-sig-001") && before_wire.contains("secret-chain-of-thought"),
            format!("剥离前同来源续接应原样回放 thinking 与 signature：{before_wire}"),
        )
        .unwrap();

        let inspect_before_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("剥离前 inspect 失败");
        let inspect_before: Value = serde_json::from_slice(&inspect_before_cmd.stdout).unwrap();
        let step1_node = inspect_before["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| {
                entry["kind"] == "step" && entry["reasoning_bytes"].as_u64().unwrap_or(0) > 0
            })
            .expect("应找到包含推理字节的首轮 step 节点")["node"]
            .as_u64()
            .unwrap();
        let step1_arg = step1_node.to_string();

        let strip_cmd = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid,
                "--strip-reasoning",
                &step1_arg,
            ])
            .expect("执行 --strip-reasoning 失败");
        evidence.record_command("context-op 剥离签名推理", &strip_cmd);
        require(strip_cmd.is_clean_exit(0), "--strip-reasoning 应退出 0").unwrap();

        let inspect_after_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("剥离后 inspect 失败");
        let inspect_after: Value = serde_json::from_slice(&inspect_after_cmd.stdout).unwrap();
        let step1_after = inspect_after["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["node"] == step1_node)
            .expect("剥离推理后该 step 文本仍应保留");
        require(
            step1_after["reasoning_bytes"] == 0 && step1_after["text_bytes"].as_u64().unwrap() > 0,
            format!("剥离后 reasoning_bytes 应归零且保留文本：{step1_after}"),
        )
        .unwrap();

        let trigger_cmd = send
            .run_command(&["--json", "session", "trigger", &sid])
            .expect("剥离后执行 trigger 失败");
        evidence.record_command("剥离后 trigger 核验无签名回放", &trigger_cmd);
        let triggered = accepted(trigger_cmd).unwrap();
        require(
            triggered.is_clean_exit(0) && triggered.reply_text() == "剥离后触发回复",
            "剥离后 trigger 应成功完成",
        )
        .unwrap();

        let captures = server.captured_requests();
        require(captures.len() == 3, "应恰好产生 3 次 Anthropic 模型请求").unwrap();
        let after_wire = captures[2].raw.to_string();
        require(
            after_wire.contains("带签名的思考回复")
                && !after_wire.contains("anth-sig-001")
                && !after_wire.contains("secret-chain-of-thought")
                && !after_wire.contains("\"type\":\"thinking\""),
            format!("--strip-reasoning 后请求体仍保留正文但不得再包含思考块或签名：{after_wire}"),
        )
        .unwrap();

        evidence.record_captures(captures);
        server
            .shutdown(spec.step_timeout())
            .expect("关闭 Anthropic 假模型服务失败");
        record_session(
            &send,
            &mut evidence,
            &sid,
            json!({"stripped_node": step1_node}),
        );
    }));
    finish_recorded(evidence, checked);
}

#[test]
fn context_op_omit_and_replace_image_recovers_text_only_model_and_32mib_budget() {
    let driver = driver();
    let (world, spec) = direct_world("context-domain-images", "vision", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let mut server = wire_fake_llm(&world, "vision", SseScript::plain("图像恢复测试回复"));

    // 为同一本地 provider 配置多模态与纯文本两个模型元数据。
    let providers_path = world.home().join(".lattice/providers.toml");
    let base_toml = std::fs::read_to_string(&providers_path).expect("读取 providers.toml 失败");
    std::fs::write(
        &providers_path,
        format!(
            "{base_toml}\n[providers.local.models.vision]\ninput = [\"text\", \"image\"]\n\n[providers.local.models.text-only]\ninput = [\"text\"]\n"
        ),
    )
    .expect("追加模型输入模态配置失败");

    std::fs::write(world.project_dir().join("shot.png"), PNG_MAGIC).expect("写入小 PNG 失败");
    std::fs::write(world.project_dir().join("photo.jpg"), JPEG_MAGIC).expect("写入小 JPEG 失败");
    let mut big = vec![0xCD; BIG_IMAGE_BYTES];
    big[..PNG_MAGIC.len()].copy_from_slice(PNG_MAGIC);
    std::fs::write(world.project_dir().join("big.png"), &big).expect("写入 3 MiB PNG 失败");

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "vision");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // === A. 切纯文本模型后不 rewind，经 --omit-image 与 --replace-image（支持规范串与 @PATH）恢复 ===
        let r1_cmd = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "保留两张历史图片",
                "--agent",
                "vision",
                "--image",
                "shot.png",
                "--image",
                "photo.jpg",
            ])
            .expect("执行含图首轮失败");
        evidence.record_command("多模态首轮发送两张图片", &r1_cmd);
        let r1 = accepted(r1_cmd).unwrap();
        let sid_a = r1.accepted_session_id().unwrap().to_owned();

        let text_only_reject = execution(
            send.run_command(&[
                "--json",
                "session",
                "send",
                "切到纯文本模型",
                "--session",
                &sid_a,
                "--model",
                "local/text-only",
            ])
            .expect("执行切纯文本模型发送失败"),
        );
        evidence.record_command("含历史图片切纯文本模型被拒", &text_only_reject.command);
        let rej = text_only_reject
            .rejection()
            .expect("纯文本模型遇到历史图片应返回 rejected");
        require(
            text_only_reject.is_clean_exit(2)
                && rej.kind == "bad_request"
                && rej.message.contains("不支持图片输入")
                && rej.message.contains("lattice session context-op"),
            format!("纯文本模型拒收应包含 context-op 引导：{rej:?}"),
        )
        .unwrap();

        let inspect_a: Value = serde_json::from_slice(
            &send
                .run_command(&["--json", "session", "inspect", &sid_a])
                .unwrap()
                .stdout,
        )
        .unwrap();
        let input_a = inspect_a["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "input")
            .unwrap()["node"]
            .as_u64()
            .unwrap();
        let png_spec = image_spec(&inspect_a["inspection"]["images"][0]);
        let input_a_arg = input_a.to_string();

        let edit_a_cmd = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid_a,
                "--omit-image",
                &input_a_arg,
                &png_spec,
                "--replace-image",
                &input_a_arg,
                "@photo.jpg",
                "此处原为风景照片",
            ])
            .expect("执行图片移出/替换失败");
        evidence.record_command("不 rewind 移出 PNG 并替换 JPEG 为文本", &edit_a_cmd);
        require(edit_a_cmd.is_clean_exit(0), "图片 context-op 应退出 0").unwrap();

        let inspect_a_after: Value = serde_json::from_slice(
            &send
                .run_command(&["--json", "session", "inspect", &sid_a])
                .unwrap()
                .stdout,
        )
        .unwrap();
        require(
            inspect_a_after["inspection"]["images"]
                .as_array()
                .unwrap()
                .is_empty()
                && inspect_a_after["inspection"]["total_image_bytes"] == 0,
            format!("清理图片后有效图片数应为 0：{inspect_a_after}"),
        )
        .unwrap();

        let trigger_a_cmd = send
            .run_command(&["--json", "session", "trigger", &sid_a])
            .expect("清理图片后触发纯文本模型失败");
        evidence.record_command("清理图片后纯文本模型 trigger 成功", &trigger_a_cmd);
        let trigger_a = accepted(trigger_a_cmd).unwrap();
        require(trigger_a.is_clean_exit(0), "纯文本模型 trigger 应退出 0").unwrap();

        let cap_text_only = server.captured_requests().pop().unwrap();
        let text_only_raw = cap_text_only.raw.to_string();
        require(
            cap_text_only.model == "text-only"
                && text_only_raw.contains("此处原为风景照片")
                && text_only_raw.contains("图像恢复测试回复")
                && !text_only_raw.contains("image_url"),
            format!("无 rewind 恢复后的纯文本模型请求应保留首轮历史且不含图片：{text_only_raw}"),
        )
        .unwrap();

        // === B. 累计超过 32 MiB 本地请求体预算后，不 rewind，经 context-op 精简历史图片直接恢复 ===
        let three_big = [
            "--image", "big.png", "--image", "big.png", "--image", "big.png",
        ];
        let run_big = |args: &[&str]| {
            let mut full = vec!["--json", "session", "send"];
            full.extend_from_slice(args);
            full.extend_from_slice(&three_big);
            send.run_command(&full).expect("执行大图发送失败")
        };

        let b1 = accepted(run_big(&["大图第一轮", "--agent", "vision"])).unwrap();
        let sid_b = b1.accepted_session_id().unwrap().to_owned();
        let b2 = accepted(run_big(&["大图第二轮", "--session", &sid_b])).unwrap();
        require(
            b1.is_clean_exit(0) && b2.is_clean_exit(0),
            "前两轮大图应成功",
        )
        .unwrap();

        let b3 = execution(run_big(&["大图第三轮超预算", "--session", &sid_b]));
        let b3_rej = b3.rejection().expect("第三轮累计超 32 MiB 应返回 rejected");
        require(
            b3.is_clean_exit(2)
                && b3_rej.kind == "bad_request"
                && b3_rej.message == BUDGET_EXCEEDED_NOTICE,
            format!("超 32 MiB 预算应返回包含 context-op 指引的固定文案：{b3_rej:?}"),
        )
        .unwrap();

        let inspect_b: Value = serde_json::from_slice(
            &send
                .run_command(&["--json", "session", "inspect", &sid_b])
                .unwrap()
                .stdout,
        )
        .unwrap();
        let input_nodes: Vec<u64> = inspect_b["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "input")
            .map(|e| e["node"].as_u64().unwrap())
            .collect();
        require(input_nodes.len() == 2, "前两轮应产生 2 个 input 节点").unwrap();
        let n1 = input_nodes[0].to_string();
        let n2 = input_nodes[1].to_string();

        let trim_cmd = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid_b,
                "--omit-image",
                &n1,
                "@big.png",
                "--replace-image",
                &n2,
                "@big.png",
                "第二轮大图已压缩为文字说明",
            ])
            .expect("执行 32 MiB 历史图片精简失败");
        evidence.record_command("不 rewind 精简前两轮大图", &trim_cmd);
        require(trim_cmd.is_clean_exit(0), "精简前两轮大图应退出 0").unwrap();

        let b3_retry = accepted(run_big(&["大图第三轮恢复重发", "--session", &sid_b])).unwrap();
        require(
            b3_retry.is_clean_exit(0),
            "精简历史图片后无 rewind 重发第三轮大图应成功",
        )
        .unwrap();

        let captures = server.captured_requests();
        let last_capture = captures.last().unwrap();
        let last_len = body_len(&last_capture.raw);
        let last_msgs = last_capture.raw["messages"].as_array().unwrap();
        let combined_text: String = last_msgs
            .iter()
            .filter_map(|m| {
                m["content"].as_str().map(str::to_owned).or_else(|| {
                    m["content"].as_array().map(|arr| {
                        arr.iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
            })
            .collect::<Vec<_>>()
            .join("\n");
        require(
            last_len < 16 * 1024 * 1024
                && combined_text.contains("大图第一轮")
                && combined_text.contains("大图第二轮")
                && combined_text.contains("第二轮大图已压缩为文字说明")
                && combined_text.contains("大图第三轮恢复重发"),
            format!("无 rewind 恢复后的请求体应保留前两轮文本历史并压回预算内（长度={last_len}）"),
        )
        .unwrap();

        let summary = json!({"request_count": captures.len(), "recovered_body_len": last_len});
        evidence
            .record_json("image-recovery-summary", &summary)
            .expect("记录图片恢复摘要失败");
        server
            .shutdown(spec.step_timeout())
            .expect("关闭假模型服务失败");
        record_session(&send, &mut evidence, &sid_b, summary);
    }));
    finish_recorded(evidence, checked);
}

#[cfg(feature = "fake-tools")]
#[test]
fn context_op_omit_invocation_and_replace_tool_result() {
    let driver = driver();
    let (world, spec) = direct_world("context-domain-tools", "tool-ctx", "");
    world.set_env("LATTICE_TEST_TOOLS", "read_note,read_second");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");

    let mut server = wire_fake_llm(
        &world,
        "tool-ctx",
        sequence(vec![
            SseScript::ToolCalls {
                calls: vec![
                    (
                        "call_1".into(),
                        "read_note".into(),
                        r#"{"action":"read","value":{"note":"note-one-secret"}}"#.into(),
                    ),
                    (
                        "call_2".into(),
                        "read_second".into(),
                        r#"{"action":"read","value":{"note":"note-two-raw"}}"#.into(),
                    ),
                ],
            },
            SseScript::plain("两份笔记均已读取"),
            SseScript::plain("编辑工具调用后的触发回复"),
        ]),
    );

    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "tool-ctx");
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let first_cmd = send
            .run_command(&[
                "--json",
                "session",
                "send",
                "同时读取两份笔记",
                "--agent",
                "tool-ctx",
            ])
            .expect("执行双工具调用发送失败");
        evidence.record_command("执行双工具调用首轮", &first_cmd);
        let first = accepted(first_cmd).unwrap();
        require(
            first.is_clean_exit(0) && first.reply_text() == "两份笔记均已读取",
            "双工具调用轮次应成功完成",
        )
        .unwrap();
        let sid = first.accepted_session_id().unwrap().to_owned();
        let turn_id = first.accepted_turn_id().unwrap().to_owned();

        let turn_cmd = send
            .run_command(&["--json", "session", "turn", &turn_id])
            .expect("查询首轮详情失败");
        evidence.record_command("查询双工具调用 InvocationId", &turn_cmd);
        let turn_detail: Value = serde_json::from_slice(&turn_cmd.stdout).unwrap();
        let slots = turn_detail["steps"][0]["slots"]
            .as_array()
            .expect("Step 0 应包含两个工具调用槽位");
        require(slots.len() == 2, format!("应恰好有 2 个槽位：{slots:?}")).unwrap();
        let inv_1 = slots[0]["invocation_id"].as_str().unwrap().to_owned();
        let inv_2 = slots[1]["invocation_id"].as_str().unwrap().to_owned();

        let op_cmd = send
            .run_command(&[
                "--json",
                "session",
                "context-op",
                &sid,
                "--omit-invocation",
                &inv_1,
                "--replace-tool-result",
                &inv_2,
                "精简后的第二条工具结果",
            ])
            .expect("执行工具调用上下文编辑失败");
        evidence.record_command("移出 call_1 并替换 call_2 工具结果", &op_cmd);
        require(op_cmd.is_clean_exit(0), "工具调用 context-op 应退出 0").unwrap();

        let inspect_cmd = send
            .run_command(&["--json", "session", "inspect", &sid])
            .expect("编辑工具后 inspect 失败");
        evidence.record_command("编辑工具后 JSON inspect", &inspect_cmd);
        let inspect: Value = serde_json::from_slice(&inspect_cmd.stdout).unwrap();
        let tool_step = inspect["inspection"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "step" && e["tool_call_count"] == 1)
            .expect("编辑后工具 Step 的有效 tool_call_count 应从 2 降为 1");
        let expected_canonical_json = serde_json::to_string("精简后的第二条工具结果").unwrap();
        require(
            tool_step["tool_result_bytes"] == expected_canonical_json.len(),
            format!("替换后的工具结果字节数应等于规范 JSON 长度：{tool_step}"),
        )
        .unwrap();

        let trigger_cmd = send
            .run_command(&["--json", "session", "trigger", &sid])
            .expect("编辑工具后执行 trigger 失败");
        evidence.record_command("编辑工具后 session trigger", &trigger_cmd);
        let triggered = accepted(trigger_cmd).unwrap();
        require(
            triggered.is_clean_exit(0) && triggered.reply_text() == "编辑工具调用后的触发回复",
            "编辑工具后 trigger 应成功完成",
        )
        .unwrap();

        let captures = server.captured_requests();
        require(captures.len() == 3, "应恰好产生 3 次模型请求").unwrap();
        let third_messages = captures[2].raw["messages"].as_array().unwrap();
        let third_messages_raw = serde_json::to_string(third_messages).unwrap();
        require(
            !third_messages_raw.contains("call_1")
                && !third_messages_raw.contains("read_note")
                && !third_messages_raw.contains("note-one-secret"),
            format!("被移出的 call_1 调用与结果均不得出现在对话消息中：{third_messages_raw}"),
        )
        .unwrap();
        let assistant_tool_calls = third_messages
            .iter()
            .find_map(|m| m["tool_calls"].as_array())
            .expect("请求体中应保留修改后的 assistant tool_calls");
        require(
            assistant_tool_calls.len() == 1
                && assistant_tool_calls[0]["id"] == "call_2"
                && assistant_tool_calls[0]["function"]["name"] == "read_second",
            format!("tool_calls 应只保留 call_2：{assistant_tool_calls:?}"),
        )
        .unwrap();
        let tool_msgs: Vec<&Value> = third_messages
            .iter()
            .filter(|m| m["role"] == "tool")
            .collect();
        require(
            tool_msgs.len() == 1
                && tool_msgs[0]["tool_call_id"] == "call_2"
                && tool_msgs[0]["content"] == expected_canonical_json
                && !tool_msgs[0].to_string().contains("note-two-raw"),
            format!("工具结果消息应保留 call_2 且载荷为规范 JSON 字符串：{tool_msgs:?}"),
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
            json!({"omitted": inv_1, "replaced": inv_2}),
        );
    }));
    finish_recorded(evidence, checked);
}
