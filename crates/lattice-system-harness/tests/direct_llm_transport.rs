// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型请求传输层（#265）的真实二进制场景：请求头、Retry-After、错误诊断、
//! 断流重试与 Turn 内连接复用。每个场景都是多次请求的 Turn，断言落在
//! fake provider 收到的请求与持久 Turn 记录上。

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{
    AssertError, CapturedRequest, FakeLlmServer, SendExecution, SessionSendDriver, SseScript,
    TurnEventLine,
};
use serde_json::Value;
use support::*;

const ACCEPT: &str = "text/event-stream, application/json";
const RETRY_FAST: &str = "[llm.retry]\nmax_retries = 3\ninitial_delay_ms = 1\nmax_delay_ms = 1\n";

fn final_turn(outcome: &SendExecution) -> Value {
    outcome
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => {
                Some(raw["view"]["selected_turn"].clone())
            }
            _ => None,
        })
        .expect("final durable snapshot")
}

/// 第 `step` 个 Step 的全部尝试记录。
fn attempts(turn: &Value, step: usize) -> Vec<Value> {
    turn["steps"][step]["attempts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn diagnostic(attempt: &Value) -> String {
    attempt["end"]["diagnostic"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn http(status: u16, content_type: &str, headers: &[(&str, &str)], body: &str) -> SseScript {
    SseScript::HttpResponse {
        status,
        content_type: content_type.into(),
        headers: headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        body: body.into(),
    }
}

/// 请求按冻结配置标识 LATTICE 与本 Session，并声明可接受 SSE 与 JSON 错误正文。
fn assert_identity(captures: &[CapturedRequest], session: &str) -> Result<(), AssertError> {
    for (index, capture) in captures.iter().enumerate() {
        let agent = capture.headers.get("user-agent").map(String::as_str);
        require(
            agent.is_some_and(|ua| ua.starts_with("lattice/") && ua.contains("(linux ")),
            format!("request {index} User-Agent identifies LATTICE: {agent:?}"),
        )?;
        require(
            capture.headers.get("accept").map(String::as_str) == Some(ACCEPT),
            format!(
                "request {index} Accept: {:?}",
                capture.headers.get("accept")
            ),
        )?;
        require(
            capture
                .headers
                .get("x-client-request-id")
                .map(String::as_str)
                == Some(session),
            format!(
                "request {index} x-client-request-id is the SessionId {session}: {:?}",
                capture.headers.get("x-client-request-id")
            ),
        )?;
    }
    Ok(())
}

#[test]
fn retry_after_is_honoured_and_requests_identify_lattice() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-transport-retry-after",
        "transport",
        r#"["observe.workspace"]"#,
        RETRY_FAST,
    );
    world
        .write_project_file("notes.txt", "transport-sentinel\n")
        .unwrap();
    let server = wire_fake_llm(
        &world,
        "transport",
        // 第一个 Step 正常执行工具；限流落在第二个 Step。
        sequence(vec![
            tool_call(
                "call-1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"notes.txt"}"#,
            ),
            http(
                429,
                "application/json",
                &[("retry-after", "2")],
                r#"{"error":{"message":"Rate limit reached for requests","type":"requests","code":"rate_limit_exceeded"}}"#,
            ),
            SseScript::plain("done"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read notes after the rate limit").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "turn completes after one retry")?;
        require(
            captures.len() == 3,
            format!("3 model requests, got {}", captures.len()),
        )?;
        let waited = captures[2].received_ms - captures[1].received_ms;
        require(
            waited >= 1_900,
            format!(
                "retry waits for provider retry-after (2s) over 1ms backoff, waited {waited}ms"
            ),
        )?;
        assert_identity(&captures, send.session_id().unwrap())?;
        require(
            captures[0].connection == captures[1].connection,
            format!(
                "the second Step reuses the first Step's connection: {} vs {}",
                captures[0].connection, captures[1].connection
            ),
        )?;
        let turn = final_turn(&outcome);
        let second = attempts(&turn, 1);
        require(
            second.len() == 2,
            format!("step 1 has 2 attempts: {second:?}"),
        )?;
        let line = diagnostic(&second[0]);
        require(
            line == "HTTP 429 rate_limit_exceeded: Rate limit reached for requests (retry after 2s)",
            format!("parsed rate-limit diagnostic: {line:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("retry-after turn", &outcome.command)],
        captures,
        result,
    );
}

#[test]
fn retry_after_beyond_limits_fails_at_once_with_the_provider_reason() {
    let driver = driver();
    // 超过 60 秒上限；或在上限内但超过 Turn 剩余时间。两种都应立即以 provider 原因结束。
    for (case, retry_after, limits) in [
        ("over-cap", "120", ""),
        ("over-deadline", "30", "[llm]\ntimeout_secs = 10\n"),
    ] {
        let (world, spec) = direct_world(
            &format!("direct-transport-long-retry-{case}"),
            "transport",
            r#"["observe.workspace"]"#,
            &format!("{limits}{RETRY_FAST}"),
        );
        world.write_project_file("notes.txt", "quota\n").unwrap();
        // 第一个 Step 正常执行工具，限流落在第二个 Step。
        let server = wire_fake_llm(
            &world,
            "transport",
            sequence(vec![
                tool_call(
                    "call-1",
                    "observe.workspace",
                    r#"{"mode":"read_file","path":"notes.txt"}"#,
                ),
                http(
                    429,
                    "application/json",
                    &[("retry-after", retry_after)],
                    r#"{"error":{"message":"You exceeded your current quota","type":"requests","code":"rate_limit_exceeded"}}"#,
                ),
            ]),
        );
        let evidence = evidence(&world, &spec);
        let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
        let started = Instant::now();
        let outcome = send.send("trigger long retry-after").unwrap();
        let elapsed = started.elapsed();
        let captures = server.captured_requests();
        let result = (|| {
            assert_clean_send(&outcome)?;
            assert_accepted_and_closed(&outcome)?;
            let turn = final_turn(&outcome);
            let end = &turn["steps"][1]["state"]["end"];
            require(
                outcome.command.is_clean_exit(1)
                    && turn["summary"]["outcome"] == "failed"
                    && end["cause"]["cause"] == "provider_error",
                format!("{case}: second Step fails with provider_error, not deadline: {end}"),
            )?;
            require(
                captures.len() == 2,
                format!(
                    "{case}: the rate-limited request is not retried, got {} requests",
                    captures.len()
                ),
            )?;
            require(
                elapsed < Duration::from_secs(8),
                format!("{case}: closes at once instead of waiting, took {elapsed:?}"),
            )?;
            let notice = turn["summary"]["notice"]["text"]
                .as_str()
                .unwrap_or_default();
            require(
                notice.starts_with("HTTP 429 rate_limit_exceeded: You exceeded your current quota")
                    && notice.contains(&format!("retry after {retry_after}s")),
                format!("{case}: notice keeps the provider reason: {notice:?}"),
            )
        })();
        finish_scenario(evidence, &[(case, &outcome.command)], captures, result);
    }
}

#[test]
fn overload_and_gateway_errors_retry_with_bounded_diagnostics() {
    let driver = driver();
    let spec = lattice_system_harness::ScenarioSpec::direct_local("direct-transport-overload");
    let world = lattice_system_harness::ScenarioWorld::new(&spec.name).expect("world");
    world
        .write_project_file(
            ".lattice/agents/overload/agent.toml",
            "schema_version = 1\n\n[agent]\nname = \"overload\"\nmodel = \"sonnet\"\n\n\
             [prompt]\nsystem = \"You are a concise test agent.\"\n\n\
             [tools]\nenabled = [\"observe.workspace\"]\n\n\
             [llm.retry]\nmax_retries = 1\ninitial_delay_ms = 1\nmax_delay_ms = 1\n",
        )
        .unwrap();
    world.write_project_file("notes.txt", "load\n").unwrap();
    world.init_git_project().unwrap();
    let gateway_page = format!(
        "<html>\r\n<head><title>502 Bad Gateway</title></head>\r\n<body>{}</body></html>",
        "x".repeat(5_000)
    );
    // 第一个 Step 正常执行工具；第二个 Step 先 529 再 502，用尽一次重试后以 502 结束 Turn。
    let server = FakeLlmServer::start(sequence(vec![
        tool_call(
            "call-1",
            "observe.workspace",
            r#"{"mode":"read_file","path":"notes.txt"}"#,
        ),
        http(
            529,
            "application/json",
            &[],
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_011CTest"}"#,
        ),
        http(502, "text/html", &[], &gateway_page),
    ]))
    .unwrap();
    let key = "anthropic-transport-key";
    server.expect_api_key(key);
    world.set_env("ANTHROPIC_BASE_URL", server.base_url());
    world.set_env("ANTHROPIC_API_KEY", key);
    let evidence =
        lattice_system_harness::ScenarioEvidence::new(&world, &spec.name, [key.to_string()])
            .unwrap();
    let send = SessionSendDriver::new(&driver, &world, &spec, "overload");
    let outcome = send.send("survive overload").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        assert_accepted_and_closed(&outcome)?;
        require(
            captures.len() == 3,
            format!("3 model requests, got {}", captures.len()),
        )?;
        let turn = final_turn(&outcome);
        require(
            outcome.command.is_clean_exit(1) && turn["summary"]["outcome"] == "failed",
            format!("turn fails once the retry is spent: {}", turn["summary"]),
        )?;
        let step = attempts(&turn, 1);
        require(
            step.len() == 2,
            format!("2 attempts in the second Step: {step:?}"),
        )?;
        let overloaded = diagnostic(&step[0]);
        require(
            overloaded == "HTTP 529 overloaded_error: Overloaded",
            format!("Anthropic error envelope parsed, request id kept out: {overloaded:?}"),
        )?;
        require(
            step[1]["end"]["cause"]
                == serde_json::json!({"cause":"provider_error","retryable":true}),
            format!(
                "502 is retryable; only the budget ends the turn: {}",
                step[1]["end"]
            ),
        )?;
        let gateway = diagnostic(&step[1]);
        require(
            gateway.starts_with("HTTP 502: <html> <head><title>502 Bad Gateway</title></head>")
                && gateway.chars().count() == 256
                && gateway.ends_with('…'),
            format!("non-JSON body is a bounded single-line excerpt: {gateway:?}"),
        )?;
        let notice = &turn["summary"]["notice"];
        require(
            notice["level"] == "error" && notice["text"] == gateway.as_str(),
            format!("the failed turn's notice is the last attempt's diagnostic: {notice}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("overload turn", &outcome.command)],
        captures,
        result,
    );
}

#[test]
fn stream_cut_before_completion_is_retried() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-transport-cut",
        "transport",
        r#"["observe.workspace"]"#,
        RETRY_FAST,
    );
    world.write_project_file("notes.txt", "cut\n").unwrap();
    // 第二个 Step 先遇到正常 EOF 的断流，再遇到声明长度后读到一半的断流，第三次成功。
    let server = wire_fake_llm(
        &world,
        "transport",
        sequence(vec![
            tool_call(
                "call-1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"notes.txt"}"#,
            ),
            SseScript::Disconnect {
                chunks_before_drop: vec!["partial".into()],
            },
            SseScript::Truncated {
                chunks_before_drop: vec!["half".into()],
            },
            SseScript::plain("whole answer"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("answer despite a cut").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(
            outcome.has_completed(),
            "cut stream is retried to completion",
        )?;
        require(
            captures.len() == 4,
            format!("4 model requests, got {}", captures.len()),
        )?;
        let turn = final_turn(&outcome);
        let step = attempts(&turn, 1);
        let retryable = serde_json::json!({"cause":"provider_error","retryable":true});
        require(
            step.len() == 3
                && step[0]["end"]["cause"] == retryable
                && step[1]["end"]["cause"] == retryable,
            format!("both cuts are retryable provider errors: {step:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("cut turn", &outcome.command)],
        captures,
        result,
    );
}

#[test]
fn multi_step_turn_reuses_one_connection_and_survives_server_close() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-transport-reuse",
        "transport",
        r#"["observe.workspace"]"#,
        RETRY_FAST,
    );
    world.write_project_file("a.txt", "alpha\n").unwrap();
    world.write_project_file("b.txt", "beta\n").unwrap();
    let server = wire_fake_llm(
        &world,
        "transport",
        sequence(vec![
            tool_call(
                "call-1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"a.txt"}"#,
            ),
            SseScript::CloseAfter {
                script: Box::new(tool_call(
                    "call-2",
                    "observe.workspace",
                    r#"{"mode":"read_file","path":"b.txt"}"#,
                )),
            },
            SseScript::plain("both read"),
        ]),
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read both files").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "three-step turn completes")?;
        require(
            captures.len() == 3,
            format!("3 model requests, got {}", captures.len()),
        )?;
        let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
        require(
            connections[0] == connections[1],
            format!("steps 0 and 1 reuse one connection: {connections:?}"),
        )?;
        require(
            connections[2] != connections[1],
            format!("after the server closes it, step 2 opens a new one: {connections:?}"),
        )?;
        let turn = final_turn(&outcome);
        for step in 0..3 {
            let records = attempts(&turn, step);
            require(
                records.len() == 1,
                format!("step {step} has exactly one attempt: {records:?}"),
            )?;
        }
        Ok(())
    })();
    finish_scenario(
        evidence,
        &[("reuse turn", &outcome.command)],
        captures,
        result,
    );
}
