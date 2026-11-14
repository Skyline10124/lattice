// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! provider 失败、`SIGINT`、截止时间超时、文件不回滚与会话忙拒绝的真实二进制场景。
mod support;
use lattice_system_harness::{
    AssertError, FAKE_API_KEY, FakeLlmServer, HarnessIntervention, ProcessTermination,
    ProviderFixture, SendExecution, SessionSendDriver, SseScript,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
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
        "失败轮次应以错误级别提示和退出码 1 收束",
    )?;
    require(
        end["end"] == "stop" && end["cause"]["cause"] == cause,
        format!("停止原因应为 {cause}：{end}"),
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
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
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
        require(
            server.captured_requests().len() == 1,
            "禁用重试时不得再次请求 provider",
        )?;
        require(detail.is_clean_exit(0), "失败轮次仍应可正常读取")?;
        let persisted: Value = serde_json::from_slice(&detail.stdout).unwrap();
        require(
            persisted["steps"][0]["attempts"][0]["origin"]
                == serde_json::json!({"protocol":"chat-completions","provider_id":"local"}),
            "失败尝试应记录协议类型与 provider 标识",
        )?;
        require(
            same_turn(&final_turn(&outcome), &persisted),
            "重新读取的轮次详情应与实时步骤条目一致",
        )?;
        let entries = outcome.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.step().is_some())
            .expect("即使尝试失败，预留的步骤条目也必须持久化");
        require(
            outcome.reply_text().is_empty() && entry.raw["content"]["reply"].is_null(),
            "失败尝试不得产生完整回复",
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
                format!("半截断流尝试必须实时流出 partial 的非空前缀：{segments:?}"),
            )
        } else {
            require(segments.is_empty(), "输出前失败不得产生任何文本片段")
        }
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("触发 provider 失败发送", &outcome.command),
            ("查询失败轮次详情", &detail),
        ],
        server.captured_requests(),
        result,
        test,
    );
}
#[cfg(feature = "fake-tools")]
#[test]
fn reported_usage_survives_failed_attempt_and_retry_without_partial_reply() {
    use serde_json::json;
    let driver = driver();
    let (world, spec) = direct_world(
        "failure-usage-retry",
        "failure",
        "[llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.set_env("LATTICE_TEST_TOOLS", "read_note");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let usage = json!({"prompt_tokens":1000,"completion_tokens":100,"prompt_tokens_details":{"cached_tokens":200,"cache_creation_input_tokens":100},"completion_tokens_details":{"reasoning_tokens":20}});
    let priced = |script| SseScript::WithUsage {
        script: Box::new(script),
        usage: lattice_system_harness::NativeUsage::ChatCompletions(usage.clone()),
    };
    let (server, gate) = wire_gated_fake_llm(
        &world,
        "failure",
        sequence(vec![
            SseScript::Chunks {
                chunks: vec![
                    json!({"choices":[{"delta":{"content":"partial"}}],"usage":usage}),
                    json!({"error":{"type":"server_error","message":"retry this attempt"}}),
                ],
            },
            priced(tool_call(
                "call",
                "read_note",
                r#"{"action":"read","value":{"note":"persisted tool result"}}"#,
            )),
            priced(SseScript::plain("complete")),
        ]),
    );
    let configuration = world.home().join(".lattice/providers.toml");
    let baseline = std::fs::read_to_string(&configuration).unwrap();
    let set_price = |prices: &str| {
        std::fs::write(
            &configuration,
            format!("{baseline}\n[providers.local.models.failure.cost]\n{prices}\n"),
        )
        .unwrap();
    };
    set_price("input = 99\noutput = 99");
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "failure");
    let running = send
        .spawn("retry after reporting usage, then read notes.txt")
        .unwrap();
    for (count, prices) in [
        (
            1,
            "input = 2\noutput = 10\ncache_read = 0.5\ncache_write = 3",
        ),
        (2, "input = 2"),
        (3, "input = 4\noutput = 20\ncache_read = 1\ncache_write = 6"),
    ] {
        gate.wait_until_stalled(count, running.remaining_timeout(Duration::from_secs(30)))
            .unwrap();
        set_price(prices);
        gate.release_next(running.remaining_timeout(Duration::from_secs(30)))
            .unwrap();
    }
    let outcome = send.wait_spawned(running).unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    set_price("input = 6\noutput = 30\ncache_read = 1.5\ncache_write = 9");
    let stopped = send.run_command(&["daemon", "stop"]).unwrap();
    world.daemon_departed().unwrap();
    world.start_daemon().expect("显式重启后台");
    let reopened = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let running = send.spawn("use the latest price").unwrap();
    gate.wait_until_stalled(4, running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    gate.release_next(running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    let next = send.wait_spawned(running).unwrap();
    let next_detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            next.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let old_detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let result = (|| {
        assert_clean_send(&outcome)?;
        assert_clean_send(&next)?;
        require(next.is_clean_exit(0), "后续新轮次退出码应为 0")?;
        require(
            outcome.has_completed()
                && next.has_completed()
                && server.captured_requests().len() == 4,
            "失败尝试应重试一次、完成工具步骤，并在重开后发出一次新请求",
        )?;
        require(
            [&detail, &stopped, &reopened, &next_detail, &old_detail]
                .iter()
                .all(|command| command.is_clean_exit(0)),
            "查询、停止后台与重开查询均应成功退出",
        )?;
        let saved: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let attempts = saved["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 2
                && attempts[0]["usage"]
                    == json!({"input":1000,"cache_read":200,"cache_write":100,"output":100,"reasoning":20})
                && attempts[0]["usage_kind"] == "partial"
                && attempts[0]["cost"]["exact"] == false
                && attempts[0]["cost"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0028).abs() < 1e-15)
                && attempts[1]["usage_kind"] == "complete"
                && attempts[1]["cost"]["exact"] == false
                && attempts[1]["cost"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0014).abs() < 1e-15),
            "中断尝试应记为部分用量；缺失单价仅省略对应计费项而不丢失已上报用量",
        )?;
        require(
            saved["steps"][1]["attempts"][0]["usage_kind"] == "complete"
                && saved["steps"][1]["attempts"][0]["cost"]["exact"] == true
                && saved["steps"][1]["attempts"][0]["cost"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0056).abs() < 1e-15),
            "下一步应按其结束时刻生效的单价计费",
        )?;
        require(
            saved["usage"]["partial_attempts"] == 1
                && saved["usage"]["kind"] == "partial"
                && saved["usage"]["cost"]["unknown_attempts"] == 0
                && saved["usage"]["cost"]["known"]["exact"] == false
                && saved["usage"]["cost"]["known"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0098).abs() < 1e-15),
            "费用汇总应独立于词元完整性标志累计",
        )?;
        require(
            saved == serde_json::from_slice::<Value>(&reopened.stdout).unwrap()
                && saved == serde_json::from_slice::<Value>(&old_detail.stdout).unwrap(),
            "修改单价、重开后台与执行新尝试都不得改写既有费用记录",
        )?;
        let latest: Value = serde_json::from_slice(&next_detail.stdout).unwrap();
        require(
            latest["steps"][0]["attempts"][0]["usage_kind"] == "complete"
                && latest["steps"][0]["attempts"][0]["cost"]["exact"] == true
                && latest["steps"][0]["attempts"][0]["cost"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0084).abs() < 1e-15),
            "重开后台后的新尝试应按最新单价计费",
        )?;
        let entries = outcome.entries();
        let entry = entries.iter().find(|entry| entry.step().is_some()).unwrap();
        let reply = &entry.raw["content"]["reply"];
        require(
            saved["steps"][0]["end"]["end"] == "continue"
                && reply.is_object()
                && !reply.to_string().contains("partial"),
            "失败尝试的半截回复不得进入会话历史",
        )?;
        require(
            same_turn(&final_turn(&outcome), &saved),
            "实时条目应与持久化尝试记录一致",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("含重试与计费的首轮发送", &outcome.command),
            ("查询首轮详情", &detail),
            ("停止后台", &stopped),
            ("后台重开后查询首轮详情", &reopened),
            ("重开后发送新轮次", &next.command),
            ("查询新轮次详情", &next_detail),
            ("再次核对首轮详情", &old_detail),
        ],
        server.captured_requests(),
        result,
        "reported_usage_survives_failed_attempt_and_retry_without_partial_reply",
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
    stopped_after_reported_usage(false);
}

#[test]
fn direct_failure_profile_deadline_while_stalled_commits_error() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-deadline-stall",
        "deadline",
        "[llm]\ntimeout_secs = 1\n",
    );
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    // 整份响应的闸门在发送响应头之前等待，用来覆盖连接打开阶段的截止时间轮询。
    let (server, gate) = wire_gated_fake_llm(&world, "deadline", SseScript::plain("never-served"));
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "deadline");
    let running = send
        .spawn("wait for response headers then deadline")
        .unwrap();
    gate.wait_until_stalled(1, running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    let outcome = send.wait_spawned(running).unwrap();
    let result = (|| {
        failed(&outcome, "deadline")?;
        require(
            outcome.reply_text().is_empty() && server.captured_requests().len() == 1,
            "等待响应头超时应只发出一次请求且无回复文本",
        )?;
        let turn = final_turn(&outcome);
        let attempts = turn["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 1
                && attempts[0]["usage"].is_null()
                && attempts[0]["usage_kind"] == "partial"
                && attempts[0]["cost"].is_null(),
            "响应打开阶段超时不应包含已上报用量或估算费用",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("等待响应头超时轮次发送", &outcome.command)],
        server.captured_requests(),
        result,
        "direct_failure_profile_deadline_while_stalled_commits_error",
    );
}

#[test]
fn direct_failure_deadline_after_reported_usage_preserves_cost() {
    stopped_after_reported_usage(true);
}

fn stopped_after_reported_usage(deadline: bool) {
    let (prefix, agent, test) = if deadline {
        (
            "direct-failure-deadline-usage",
            "deadline",
            "direct_failure_deadline_after_reported_usage_preserves_cost",
        )
    } else {
        (
            "direct-failure-sigint-stall",
            "cancel",
            "direct_failure_sigint_while_stalled_commits_cancelled",
        )
    };
    let driver = driver();
    // 请求预算需容纳分段交付与公开观察确认，再在终态帧被阻塞时等待截止时间到期。
    let (world, spec) = direct_world(
        prefix,
        agent,
        if deadline {
            "[llm]\ntimeout_secs = 5\n"
        } else {
            ""
        },
    );
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
    let (server, gate) = FakeLlmServer::start_segmented(
        SseScript::Chunks { chunks: vec![
            json!({"choices":[],"usage":{"prompt_tokens":1000,"completion_tokens":100,"prompt_tokens_details":{"cached_tokens":200,"cache_creation_input_tokens":100},"completion_tokens_details":{"reasoning_tokens":20}}}),
            json!({"choices":[{"delta":{"content":"usage-delivered"}}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ] },
        spec.step_timeout(),
    ).unwrap();
    world
        .wire_provider(
            ProviderFixture::chat_completions(agent),
            &server,
            FAKE_API_KEY,
        )
        .unwrap();
    let configuration = world.home().join(".lattice/providers.toml");
    let source = std::fs::read_to_string(&configuration).unwrap();
    std::fs::write(&configuration, format!("{source}\n[providers.local.models.{agent}.cost]\ninput = 2\noutput = 10\ncache_read = 0.5\ncache_write = 3\n")).unwrap();
    let mut evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, agent);
    let mut running = send.spawn("report usage then stall").unwrap();
    gate.wait_until_stalled(1, running.remaining_timeout(Duration::from_secs(30)))
        .unwrap();
    let list = send.run_command(&["--json", "session", "list"]).unwrap();
    assert!(list.is_clean_exit(0), "{}", list.summary());
    let sessions: Value = serde_json::from_slice(&list.stdout).unwrap();
    let [session] = sessions.as_array().unwrap().as_slice() else {
        panic!("{sessions}")
    };
    let sid = session["session_id"].as_str().unwrap();
    let mut watcher = Watcher::open(
        &world,
        running.remaining_timeout(Duration::from_secs(30)),
        sid,
    );
    watcher.initial();
    for delivered in 1..=2 {
        gate.release_next(running.remaining_timeout(Duration::from_secs(30)))
            .unwrap();
        gate.wait_until_stalled(
            delivered + 1,
            running.remaining_timeout(Duration::from_secs(30)),
        )
        .unwrap();
    }
    // 文字在用量之后：观察到它即可确认用量已被产品消费，终态帧仍被阻塞。
    watcher.until(
        Instant::now() + running.remaining_timeout(Duration::from_secs(30)),
        "收到用量之后的流式文本",
        |view| {
            view.texts
                .values()
                .flatten()
                .any(|segment| segment.text == "usage-delivered")
        },
    );
    evidence
        .record_json("usage-delivered-view", &json!(&watcher.received))
        .unwrap();
    drop(watcher);
    if !deadline {
        running.interrupt().unwrap();
    }
    let outcome = send.wait_spawned(running).unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let stopped = send.run_command(&["daemon", "stop"]).unwrap();
    world.daemon_departed().unwrap();
    world.start_daemon().expect("显式重启后台");
    let reopened = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let result = (|| {
        if deadline {
            failed(&outcome, "deadline")?;
        } else {
            require(
                outcome.command.termination == Some(ProcessTermination::Exited(2)),
                "协作式取消的退出码应为 2",
            )?;
            require(
                outcome.command.interventions == vec![HarnessIntervention::InterruptRequested]
                    && !outcome.command.cleanup.forced
                    && outcome.command.cleanup.succeeded()
                    && outcome.command.stdout_complete
                    && outcome.command.stderr_complete,
                "应仅触发 SIGINT 干预、完整读完输出并清理干净进程组",
            )?;
            assert_accepted_and_closed(&outcome)?;
        }
        require(
            outcome.malformed_lines.is_empty() && outcome.reply_text().is_empty(),
            "不得出现畸形记录或伪造的完整回复文本",
        )?;
        let turn = final_turn(&outcome);
        require(
            turn["summary"]["outcome"] == if deadline { "failed" } else { "cancelled" }
                && turn["summary"]["notice"]["level"] == if deadline { "error" } else { "info" }
                && turn["steps"][0]["end"]["cause"]["cause"]
                    == if deadline { "deadline" } else { "cancelled" },
            "真实的超时或取消结局必须已持久化",
        )?;
        let attempts = turn["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 1 && server.captured_requests().len() == 1,
            "应当只有一次中止尝试与一次 provider 请求",
        )?;
        require(
            attempts[0]["usage"]
                == json!({"input":1000,"cache_read":200,"cache_write":100,"output":100,"reasoning":20})
                && attempts[0]["usage_kind"] == "partial"
                && attempts[0]["cost"]["exact"] == false
                && attempts[0]["cost"]["usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0028).abs() < 1e-15),
            "中止尝试应保留已上报用量及其估算费用",
        )?;
        require(
            detail.is_clean_exit(0)
                && stopped.is_clean_exit(0)
                && reopened.is_clean_exit(0)
                && serde_json::from_slice::<Value>(&detail.stdout).unwrap()
                    == serde_json::from_slice::<Value>(&reopened.stdout).unwrap()
                && same_turn(
                    &turn,
                    &serde_json::from_slice::<Value>(&detail.stdout).unwrap(),
                ),
            "中止尝试的计量与结局在后台重开后应保持不变，且不额外发起 provider 请求",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("查询运行中会话列表", &list),
            ("收到用量后中止的发送", &outcome.command),
            ("查询中止轮次详情", &detail),
            ("停止后台", &stopped),
            ("后台重开后查询轮次详情", &reopened),
        ],
        server.captured_requests(),
        result,
        test,
    );
}
#[cfg(feature = "fake-tools")]
#[test]
fn direct_failure_mutation_survives_provider_failure() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-mutation-survives",
        "mutation",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world.set_env("LATTICE_TEST_TOOLS", "write_marker");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let survivor_path = world
        .project_dir()
        .join("survivor.txt")
        .display()
        .to_string();
    let create_args = serde_json::to_string(&json!({
        "action": "append_marker",
        "path": survivor_path,
        "line": "bytes\n",
    }))
    .unwrap();
    let server = wire_fake_llm(
        &world,
        "mutation",
        sequence(vec![
            tool_call("c1", "write_marker", &create_args),
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
            "第一步工具结果已持久提交，随后第二步请求失败",
        )?;
        require(
            std::fs::read_to_string(world.project_dir().join("survivor.txt")).unwrap() == "bytes\n",
            "已写出的标记文件在后续 provider 失败后保持不变",
        )?;
        require(
            server.captured_requests().len() == 2,
            "只发出工具轮与失败轮两次请求",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("工具写文件后触发失败的发送", &outcome.command)],
        server.captured_requests(),
        result,
        "direct_failure_mutation_survives_provider_failure",
    );
}
#[cfg(feature = "fake-tools")]
#[test]
fn direct_failure_tool_error_projects_to_model_and_continues_next_step() {
    use lattice_system_harness::{SlotOutcome, SlotState};
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-tool-error", "tool-fail", "");
    world.set_env("LATTICE_TEST_TOOLS", "check_gate");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");
    let server = wire_fake_llm(
        &world,
        "tool-fail",
        sequence(vec![
            tool_call(
                "call-fail",
                "check_gate",
                r#"{"action":"fail","code":"gate_denied","message":"simulated tool failure"}"#,
            ),
            SseScript::plain("recovered after tool error"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "tool-fail");
    let outcome = send.send("run failing tool then recover").unwrap();
    let detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            outcome.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            outcome.command.is_clean_exit(0)
                && outcome.has_completed()
                && outcome.reply_text() == "recovered after tool error",
            format!(
                "工具返回失败结果后仍应推进下一步并正常完成：{}",
                outcome.reply_text()
            ),
        )?;
        require(
            captures.len() == 2,
            format!("预期两次模型请求，实际为 {}", captures.len()),
        )?;
        let slots = outcome.slots();
        require(
            slots.len() == 1
                && slots[0].method == "check_gate"
                && slots[0].state == SlotState::Terminal
                && slots[0].outcome == SlotOutcome::Failed
                && slots[0].error_code.as_deref() == Some("gate_denied"),
            format!("公开槽位记录应处于失败终态且携带错误码 gate_denied：{slots:?}"),
        )?;
        let expected_wire =
            r#"{"error":{"code":"gate_denied","message":"simulated tool failure"}}"#;
        require(
            captures[1].raw["messages"][3]
                == json!({
                    "role": "tool",
                    "tool_call_id": "call-fail",
                    "content": expected_wire,
                }),
            format!(
                "第二步请求中的工具错误投影不匹配：{}",
                captures[1].raw["messages"][3]
            ),
        )?;
        require(detail.is_clean_exit(0), "查询轮次详情应返回退出码 0")?;
        let persisted: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let slot = &persisted["steps"][0]["slots"][0];
        let expected_reason = json!({"kind": "failed", "value": {"code": "gate_denied"}});
        let expected_material = json!({"kind": "no_content"});
        require(
            persisted["summary"]["outcome"] == "completed"
                && persisted["steps"].as_array().unwrap().len() == 2
                && persisted["steps"][0]["end"] == json!({"end": "continue"})
                && persisted["steps"][1]["end"] == json!({"end": "finish"})
                && slot["outcome"] == "failed"
                && slot["progress"]
                    == json!({
                        "state": "terminal",
                        "outcome": "failed",
                        "reason": expected_reason,
                        "material": expected_material,
                    })
                && slot["reason"] == expected_reason
                && slot["material"] == expected_material
                && slot["binding"]["method"] == "check_gate"
                && slot["binding"]["tool_identity"] == "fake:check_gate@1"
                && same_turn(&final_turn(&outcome), &persisted),
            format!("持久化轮次详情与槽位失败结论不一致：{persisted}"),
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("工具报错后恢复的轮次发送", &outcome.command),
            ("查询轮次详情", &detail),
        ],
        captures,
        result,
        "direct_failure_tool_error_projects_to_model_and_continues_next_step",
    );
}
#[test]
fn direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-stale-parent", "busy", "");
    world.budget(&spec);
    world.start_daemon().expect("显式启动后台");
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
                "已接受的轮次应正常完成",
            )?;
        }
        assert_clean_send(&racer)?;
        require(
            racer.command.is_clean_exit(1)
                && racer.accepted_turn_id().is_none()
                && rejected_kind(&racer) == Some("busy"),
            "会话忙时应直接拒绝而不接受轮次",
        )?;
        require(
            before_retry.is_empty() && racer_server.captured_requests().len() == 1,
            "会话忙拒绝绝不能排队或请求 provider",
        )?;
        let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
        let rows = tree["nodes"].as_array().unwrap();
        require(
            log.is_clean_exit(0) && rows.len() == 7,
            "会话树应包含根节点与恰好三轮完整对话",
        )?;
        let previous = final_turn(&a)["steps"][0]["node"].clone();
        require(
            rows.iter().any(|r| {
                r["turn_start"]["turn_id"] == b.accepted_turn_id().unwrap()
                    && r["node"]["parent"] == previous
            }),
            "轮次 B 的用户节点父指针应指向轮次 A 的末节点，而非旧头节点",
        )
    })();
    let mut captures = baseline_server.captured_requests();
    captures.extend(gated.captured_requests());
    captures.extend(racer_server.captured_requests());
    finish_failure(
        evidence,
        &send,
        &[
            ("基线轮次发送", &baseline.command),
            ("并发争用触发忙拒绝", &racer.command),
            ("占用会话的轮次 A", &a.command),
            ("释放后重试轮次 B", &b.command),
            ("查询会话树", &log),
        ],
        captures,
        result,
        "direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry",
    );
}

#[cfg(feature = "fake-tools")]
#[test]
fn tool_call_deadline_leaves_slot_started_and_stops_turn() {
    use lattice_system_harness::{SlotOutcome, SlotState};

    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-tool-deadline",
        "tool-deadline",
        "[llm]\ntimeout_secs = 2\n",
    );
    world.set_env("LATTICE_TEST_TOOLS", "hold_call,write_marker");
    world.budget(&spec);
    world.start_daemon().expect("显式启动带测试工具的后台");

    let ready_path = world.root().join("hold.ready");
    let marker_path = world.root().join("hold.marker");
    let skipped_path = world.project_dir().join("skipped.marker");
    let hold_args = serde_json::to_string(&json!({
        "action": "block",
        "ready_path": ready_path,
        "marker_path": marker_path,
        "marker_line": "hold-entered\n",
    }))
    .unwrap();
    let write_args = serde_json::to_string(&json!({
        "action": "append_marker",
        "path": skipped_path,
        "line": "must-not-run\n",
    }))
    .unwrap();

    let server = wire_fake_llm(
        &world,
        "tool-deadline",
        sequence(vec![
            SseScript::ToolCalls {
                calls: vec![
                    ("call-hold".into(), "hold_call".into(), hold_args),
                    ("call-write".into(), "write_marker".into(), write_args),
                ],
            },
            SseScript::plain("healthy after tool deadline"),
        ]),
    );

    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "tool-deadline");

    let outcome = send.send("block inside tool until turn deadline").unwrap();
    let requests_after_deadline = server.captured_requests();
    let first_tid = outcome.accepted_turn_id().unwrap();
    let detail = send
        .run_command(&["--json", "session", "turn", first_tid])
        .unwrap();

    let followup = send.send("follow up after deadline").unwrap();
    let followup_detail = send
        .run_command(&[
            "--json",
            "session",
            "turn",
            followup.accepted_turn_id().unwrap(),
        ])
        .unwrap();
    let detail_after = send
        .run_command(&["--json", "session", "turn", first_tid])
        .unwrap();

    let captures = server.captured_requests();
    let result = (|| {
        failed(&outcome, "deadline")?;
        require(
            requests_after_deadline.len() == 1,
            format!(
                "工具调用期超时后仅应有 1 次模型请求，实际为 {}",
                requests_after_deadline.len()
            ),
        )?;
        require(
            ready_path.exists() && marker_path.exists(),
            "hold_call 必须已进入执行并写出就绪与标记文件",
        )?;
        let ready_lines: Vec<_> = std::fs::read_to_string(&ready_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let marker_lines: Vec<_> = std::fs::read_to_string(&marker_path)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        require(
            ready_lines.len() == 1
                && ready_lines[0].starts_with("hold_call:")
                && marker_lines == vec!["hold-entered"],
            format!("hold_call 只应进入一次：就绪文件={ready_lines:?}，标记文件={marker_lines:?}"),
        )?;
        require(
            !skipped_path.exists(),
            "后续槽位 write_marker 不得执行或创建目标文件",
        )?;

        let live_slots = outcome.slots();
        require(
            live_slots.len() == 2
                && live_slots[0].method == "hold_call"
                && live_slots[0].state == SlotState::Started
                && live_slots[0].outcome == SlotOutcome::Indeterminate
                && live_slots[1].method == "write_marker"
                && live_slots[1].state == SlotState::Registered
                && live_slots[1].outcome == SlotOutcome::NotExecuted,
            format!("实时槽位阶段与结局不匹配：{live_slots:?}"),
        )?;

        require(
            detail.is_clean_exit(0)
                && followup_detail.is_clean_exit(0)
                && detail_after.is_clean_exit(0),
            "轮次详情查询应全部返回退出码 0",
        )?;
        let persisted: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let expected_end = json!({"end": "stop", "cause": {"cause": "deadline"}});
        require(
            persisted["summary"]["outcome"] == "failed"
                && persisted["summary"]["notice"]
                    == json!({"level": "error", "text": "turn deadline exceeded"})
                && persisted["steps"].as_array().unwrap().len() == 1
                && persisted["steps"][0]["end"] == expected_end,
            format!("持久化轮次的结局、提示或步骤结束原因不匹配：{persisted}"),
        )?;
        let slots = persisted["steps"][0]["slots"].as_array().unwrap();
        require(slots.len() == 2, format!("预期 2 个槽位：{slots:?}"))?;
        require(
            slots[0]["binding"]["method"] == "hold_call"
                && slots[0]["binding"]["tool_identity"] == "fake:hold_call@1"
                && slots[0]["progress"] == json!({"state": "started"})
                && slots[0]["outcome"] == "indeterminate"
                && slots[0]["reason"] == json!({"kind": "indeterminate", "value": "no_result"})
                && slots[0]["material"] == json!({"kind": "no_content"}),
            format!("槽位 0 应停留在已启动状态并读出未决结果：{}", slots[0]),
        )?;
        require(
            slots[1]["binding"]["method"] == "write_marker"
                && slots[1]["binding"]["tool_identity"] == "fake:write_marker@1"
                && slots[1]["progress"] == json!({"state": "registered"})
                && slots[1]["outcome"] == "not_executed"
                && slots[1]["reason"].is_null()
                && slots[1]["material"] == json!({"kind": "no_content"}),
            format!("槽位 1 应停留在已登记状态并读出未执行结果：{}", slots[1]),
        )?;
        require(
            same_turn(&final_turn(&outcome), &persisted),
            "实时条目与持久化轮次详情应一致",
        )?;

        assert_clean_send(&followup)?;
        assert_accepted_and_closed(&followup)?;
        require(
            followup.command.is_clean_exit(0)
                && followup.has_completed()
                && followup.reply_text() == "healthy after tool deadline"
                && followup.accepted_session_id() == outcome.accepted_session_id(),
            format!("同一会话后续纯文本轮应正常完成：{}", followup.summary()),
        )?;
        require(
            captures.len() == 2,
            format!(
                "后续纯文本轮完成后总模型请求数应为 2，实际为 {}",
                captures.len()
            ),
        )?;
        require(
            !skipped_path.exists()
                && std::fs::read_to_string(&marker_path).unwrap() == "hold-entered\n"
                && serde_json::from_slice::<Value>(&detail_after.stdout).unwrap() == persisted,
            "后续轮次不得重放旧工具调用或改写已收束的超时轮次",
        )
    })();

    finish_failure(
        evidence,
        &send,
        &[
            ("工具调用期超时轮次发送", &outcome.command),
            ("查询超时轮次详情", &detail),
            ("超时后发送后续轮次", &followup.command),
            ("查询后续轮次详情", &followup_detail),
            ("后续轮次后复查超时轮次详情", &detail_after),
        ],
        captures,
        result,
        "tool_call_deadline_leaves_slot_started_and_stops_turn",
    );
}
