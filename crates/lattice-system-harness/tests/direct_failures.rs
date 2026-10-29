// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! provider 失败、SIGINT、deadline、文件不回滚与 Busy 的真实二进制场景。
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
        "Failed/Error with exit 1",
    )?;
    require(
        end["end"] == "stop" && end["cause"]["cause"] == cause,
        format!("Stop cause {cause}: {end}"),
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
        "[]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
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
        require(server.captured_requests().len() == 1, "no provider retry")?;
        require(detail.is_clean_exit(0), "failed Turn remains readable")?;
        let persisted: Value = serde_json::from_slice(&detail.stdout).unwrap();
        require(
            persisted["steps"][0]["attempts"][0]["origin"]
                == serde_json::json!({"protocol":"chat-completions","provider_id":"local"}),
            "failed attempt records protocol and provider id",
        )?;
        require(
            same_turn(&final_turn(&outcome), &persisted),
            "reloaded detail equals the live Step entries",
        )?;
        let entries = outcome.entries();
        let entry = entries
            .iter()
            .find(|entry| entry.step().is_some())
            .expect("reserved Step persists even on failure");
        require(
            outcome.reply_text().is_empty() && entry.raw["content"]["reply"].is_null(),
            "failed attempt does not produce a complete reply",
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
                format!("partial attempt must stream a nonempty prefix of partial: {segments:?}"),
            )
        } else {
            require(
                segments.is_empty(),
                "pre-output failure must have no text segments",
            )
        }
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("provider failure", &outcome.command),
            ("turn detail", &detail),
        ],
        server.captured_requests(),
        result,
        test,
    );
}
#[test]
fn reported_usage_survives_failed_attempt_and_retry_without_partial_reply() {
    use serde_json::json;
    let driver = driver();
    let (world, spec) = direct_world(
        "failure-usage-retry",
        "failure",
        "[\"observe__workspace\"]",
        "[llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    world
        .write_project_file("notes.txt", "persisted tool result\n")
        .unwrap();
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
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
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
        require(next.is_clean_exit(0), "new turn exits 0")?;
        require(
            outcome.has_completed()
                && next.has_completed()
                && server.captured_requests().len() == 4,
            "failed attempt retried once, tool step completed, and one new request sent",
        )?;
        require(
            [&detail, &stopped, &reopened, &next_detail, &old_detail]
                .iter()
                .all(|command| command.is_clean_exit(0)),
            "query, stop and reopen succeed",
        )?;
        let saved: Value = serde_json::from_slice(&detail.stdout).unwrap();
        let attempts = saved["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 2
                && attempts[0]["usage"]
                    == json!({"input":1000,"cache_read":200,"cache_write":100,"output":100,"reasoning":20})
                && attempts[0]["estimated_cost_usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0028).abs() < 1e-15)
                && attempts[1]["estimated_cost_usd"].is_null(),
            "failed attempt retains counters and end-time price; missing base price stays unknown",
        )?;
        require(
            saved["steps"][1]["attempts"][0]["estimated_cost_usd"]
                .as_f64()
                .is_some_and(|fee| (fee - 0.0056).abs() < 1e-15),
            "next step uses its end-time price",
        )?;
        require(
            saved["usage"]["estimated_cost_usd"]["unknown_attempts"] == 1
                && saved["usage"]["estimated_cost_usd"]["known"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0084).abs() < 1e-15),
            "fees aggregate independently of token completeness",
        )?;
        require(
            saved == serde_json::from_slice::<Value>(&reopened.stdout).unwrap()
                && saved == serde_json::from_slice::<Value>(&old_detail.stdout).unwrap(),
            "repricing, reopening and a new attempt do not rewrite prior fees",
        )?;
        let latest: Value = serde_json::from_slice(&next_detail.stdout).unwrap();
        require(
            latest["steps"][0]["attempts"][0]["estimated_cost_usd"]
                .as_f64()
                .is_some_and(|fee| (fee - 0.0084).abs() < 1e-15),
            "new attempt after reopening uses the latest price",
        )?;
        let entries = outcome.entries();
        let entry = entries.iter().find(|entry| entry.step().is_some()).unwrap();
        let reply = &entry.raw["content"]["reply"];
        require(
            saved["steps"][0]["end"]["end"] == "continue"
                && reply.is_object()
                && !reply.to_string().contains("partial"),
            "partial reply does not enter history",
        )?;
        require(
            same_turn(&final_turn(&outcome), &saved),
            "live entries match persisted attempts",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("retry turn", &outcome.command),
            ("turn detail", &detail),
            ("stop", &stopped),
            ("reopened", &reopened),
            ("new turn", &next.command),
            ("new detail", &next_detail),
            ("old detail", &old_detail),
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
        "[]",
        "[llm]\ntimeout_secs = 1\n",
    );
    // 整份响应的 gate 在发送响应头之前等待，保护打开阶段的 deadline 轮询。
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
            "one request waiting for response headers, no text",
        )?;
        let turn = final_turn(&outcome);
        let attempts = turn["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 1
                && attempts[0]["usage"].is_null()
                && attempts[0]["estimated_cost_usd"].is_null(),
            "opening deadline has no reported usage or estimated cost",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("opening deadline", &outcome.command)],
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
    // 请求预算需容纳分段交付与公开观察确认，再在终态帧被阻塞时等待 deadline。
    let (world, spec) = direct_world(
        prefix,
        agent,
        "[]",
        if deadline {
            "[llm]\ntimeout_secs = 5\n"
        } else {
            ""
        },
    );
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
        "text after reported usage",
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
                "cooperative cancellation exits 2",
            )?;
            require(
                outcome.command.interventions == vec![HarnessIntervention::InterruptRequested]
                    && !outcome.command.cleanup.forced
                    && outcome.command.cleanup.succeeded()
                    && outcome.command.stdout_complete
                    && outcome.command.stderr_complete,
                "SIGINT only, complete output and group cleanup",
            )?;
            assert_accepted_and_closed(&outcome)?;
        }
        require(
            outcome.malformed_lines.is_empty() && outcome.reply_text().is_empty(),
            "no malformed records or fabricated text",
        )?;
        let turn = final_turn(&outcome);
        require(
            turn["summary"]["outcome"] == if deadline { "failed" } else { "cancelled" }
                && turn["summary"]["notice"]["level"] == if deadline { "error" } else { "info" }
                && turn["steps"][0]["end"]["cause"]["cause"]
                    == if deadline { "deadline" } else { "cancelled" },
            "true deadline or cancellation outcome is durable",
        )?;
        let attempts = turn["steps"][0]["attempts"].as_array().unwrap();
        require(
            attempts.len() == 1 && server.captured_requests().len() == 1,
            "one stopped attempt and one provider request",
        )?;
        require(
            attempts[0]["usage"]
                == json!({"input":1000,"cache_read":200,"cache_write":100,"output":100,"reasoning":20})
                && attempts[0]["estimated_cost_usd"]
                    .as_f64()
                    .is_some_and(|fee| (fee - 0.0028).abs() < 1e-15),
            "stopped attempt keeps reported usage and its estimated cost",
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
            "stopped attempt metering and outcome survive backend reopening without another provider request",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[
            ("in-flight sessions", &list),
            ("stopped request", &outcome.command),
            ("reloaded Turn", &detail),
            ("stop backend", &stopped),
            ("reopened Turn", &reopened),
        ],
        server.captured_requests(),
        result,
        test,
    );
}
#[test]
fn direct_failure_mutation_survives_provider_failure() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-failure-mutation-survives",
        "mutation",
        "[\"mutate__create\"]",
        "[llm.retry]\nmax_retries = 0\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
    );
    let server = wire_fake_llm(
        &world,
        "mutation",
        sequence(vec![
            tool_call(
                "c1",
                "mutate__create",
                r#"{"path":"survivor.txt","content":"bytes\n","create_parents":false}"#,
            ),
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
            "mutation committed before failed next request",
        )?;
        require(
            std::fs::read_to_string(world.project_dir().join("survivor.txt")).unwrap() == "bytes\n",
            "installed bytes survive provider failure",
        )?;
        require(
            server.captured_requests().len() == 2,
            "tool round then failed request only",
        )
    })();
    finish_failure(
        evidence,
        &send,
        &[("mutation then failure", &outcome.command)],
        server.captured_requests(),
        result,
        "direct_failure_mutation_survives_provider_failure",
    );
}
#[test]
fn direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry() {
    let driver = driver();
    let (world, spec) = direct_world("direct-failure-stale-parent", "busy", "[]", "");
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
                "accepted Turn completes",
            )?;
        }
        assert_clean_send(&racer)?;
        require(
            racer.command.is_clean_exit(1)
                && racer.accepted_turn_id().is_none()
                && rejected_kind(&racer) == Some("busy"),
            "Busy rejects without acceptance",
        )?;
        require(
            before_retry.is_empty() && racer_server.captured_requests().len() == 1,
            "Busy never queues or calls provider",
        )?;
        let tree: Value = serde_json::from_slice(&log.stdout).unwrap();
        let rows = tree["nodes"].as_array().unwrap();
        require(
            log.is_clean_exit(0) && rows.len() == 7,
            "root plus exactly three complete Turns",
        )?;
        let previous = final_turn(&a)["steps"][0]["node"].clone();
        require(
            rows.iter().any(|r| {
                r["turn_start"]["turn_id"] == b.accepted_turn_id().unwrap()
                    && r["node"]["parent"] == previous
            }),
            "B User is parented to A final node, not stale head",
        )
    })();
    let mut captures = baseline_server.captured_requests();
    captures.extend(gated.captured_requests());
    captures.extend(racer_server.captured_requests());
    finish_failure(
        evidence,
        &send,
        &[
            ("baseline", &baseline.command),
            ("Busy", &racer.command),
            ("A", &a.command),
            ("retry B", &b.command),
            ("tree", &log),
        ],
        captures,
        result,
        "direct_failure_session_lock_rejects_busy_then_accepts_explicit_retry",
    );
}
