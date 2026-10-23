// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型请求传输层（#265）的真实二进制场景：请求头、Retry-After、错误诊断、
//! 断流重试、Turn 内连接复用、HTTP/2 对未处理请求的重发，以及明文 HTTP/1.1 的本地 provider。断言落在
//! fake provider 收到的请求与持久 Turn 记录上。

mod support;

use std::time::{Duration, Instant};

use lattice_system_harness::{
    AssertError, CapturedRequest, FAKE_API_KEY, FakeLlmServer, SendExecution, SessionSendDriver,
    SseScript,
};
use serde_json::Value;
use support::*;

const ACCEPT: &str = "text/event-stream, application/json";
const RETRY_FAST: &str = "[llm.retry]\nmax_retries = 3\ninitial_delay_ms = 1\nmax_delay_ms = 1\n";

fn final_turn(outcome: &SendExecution) -> Value {
    live_turn(outcome)
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
        r#"["observe__workspace"]"#,
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
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
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
            r#"["observe__workspace"]"#,
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
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
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
            let end = &turn["steps"][1]["end"];
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
             [tools]\nenabled = [\"observe__workspace\"]\n\n\
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
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
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
fn unauthorized_request_fails_without_retry_notice() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-transport-unauthorized",
        "transport",
        "[]",
        RETRY_FAST,
    );
    let server = wire_fake_llm(
        &world,
        "transport",
        SseScript::HttpError {
            status: 401,
            body: r#"{"error":{"message":"invalid credential"}}"#.into(),
        },
    );
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("reject unauthorized request").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        assert_clean_send(&outcome)?;
        require(
            outcome.command.is_clean_exit(1),
            "unauthorized Turn exits 1",
        )?;
        require(captures.len() == 1, "401 must not be retried")?;
        require(
            outcome
                .entries()
                .iter()
                .filter_map(|entry| entry.step())
                .all(|step| step.retries.is_empty()),
            "401 must not carry a retry notice",
        )?;
        let turn = final_turn(&outcome);
        let attempts = attempts(&turn, 0);
        let notice = &turn["summary"]["notice"];
        require(
            attempts.len() == 1
                && notice["level"] == "error"
                && notice["text"] == diagnostic(&attempts[0])
                && diagnostic(&attempts[0]).starts_with("HTTP 401"),
            "terminal error notice must preserve the 401 diagnostic",
        )
    })();
    finish_scenario(
        evidence,
        &[("unauthorized turn", &outcome.command)],
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
        r#"["observe__workspace"]"#,
        RETRY_FAST,
    );
    world.write_project_file("notes.txt", "cut\n").unwrap();
    // 第二个 Step 先遇到正常结束却没有结束事件的流，再遇到读到一半被
    // `RST_STREAM(INTERNAL_ERROR)` 重置的流，第三次成功。
    let server = wire_fake_llm(
        &world,
        "transport",
        sequence(vec![
            tool_call(
                "call-1",
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
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
        // 两次断流都发生在响应体中途，两次重试提示都在这一步的条目中。
        let entries = outcome.entries();
        let cut = entries
            .iter()
            .find(|entry| entry.step().is_some_and(|step| step.ordinal == 1))
            .expect("second Step entry");
        let retried: Vec<u64> = cut
            .step()
            .unwrap()
            .retries
            .iter()
            .map(|r| r.attempt)
            .collect();
        require(
            retried == [0, 1],
            format!("both stopped attempts have retry notices: {retried:?}"),
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
        r#"["observe__workspace"]"#,
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
                "observe__workspace",
                r#"{"request":{"mode":"read_file","path":"a.txt"}}"#,
            ),
            SseScript::CloseAfter {
                script: Box::new(tool_call(
                    "call-2",
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"b.txt"}}"#,
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
            format!("after the server's GOAWAY, step 2 opens a new one: {connections:?}"),
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

/// HTTP/2 下 provider 明确未处理的请求（GOAWAY 的最后流号小于它，或 `REFUSED_STREAM`）
/// 由传输层重发，归在同一次尝试里：provider 收到两次，持久记录只有一条尝试。
#[test]
fn unprocessed_requests_are_resent_inside_one_attempt() {
    for (case, refusal) in [
        ("goaway", SseScript::GoawayUnprocessed),
        ("refused", SseScript::RefusedStream),
    ] {
        let driver = driver();
        // 本地重试预算为 0：若重发没有发生在传输层，第二个 Step 只能失败。
        let (world, spec) = direct_world(
            &format!("direct-transport-unprocessed-{case}"),
            "transport",
            r#"["observe__workspace"]"#,
            "[llm.retry]\nmax_retries = 0\n",
        );
        world.write_project_file("notes.txt", "nack\n").unwrap();
        // 第一个 Step 正常执行工具；第二个 Step 的请求先被拒绝，紧接着的重发得到回答。
        let server = wire_fake_llm(
            &world,
            "transport",
            sequence(vec![
                tool_call(
                    "call-1",
                    "observe__workspace",
                    r#"{"request":{"mode":"read_file","path":"notes.txt"}}"#,
                ),
                refusal,
                SseScript::plain("answered after resend"),
            ]),
        );
        let evidence = evidence(&world, &spec);
        let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
        let outcome = send.send("answer after a refusal").unwrap();
        let captures = server.captured_requests();
        let result = (|| {
            assert_clean_send(&outcome)?;
            require(
                outcome.has_completed(),
                format!("{case}: the turn completes"),
            )?;
            require(
                captures.len() == 3,
                format!(
                    "{case}: 3 requests reach the provider, got {}",
                    captures.len()
                ),
            )?;
            require(
                captures[1].raw == captures[2].raw,
                format!("{case}: the resend carries the same request body"),
            )?;
            let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
            require(
                connections[0] == connections[1],
                format!("{case}: the refused request rode the Step 0 connection: {connections:?}"),
            )?;
            if case == "goaway" {
                require(
                    connections[2] != connections[1],
                    format!("goaway: the resend opens a new connection: {connections:?}"),
                )?;
            }
            let turn = final_turn(&outcome);
            for step in 0..2 {
                let records = attempts(&turn, step);
                require(
                    records.len() == 1,
                    format!("{case}: step {step} has exactly one attempt: {records:?}"),
                )?;
            }
            Ok(())
        })();
        finish_scenario(
            evidence,
            &[("unprocessed turn", &outcome.command)],
            captures,
            result,
        );
    }
}

/// 本地 provider（Ollama 一类）走明文 HTTP/1.1：多 Step 的 Turn 完成，服务忙的 503 按可重试处理，
/// 请求头与 HTTPS provider 相同。分块响应的结尾晚于结束事件到达，整个 Turn 仍只用一条连接：
/// 产品交付结束事件前读完了响应体，连接才能回到连接池。
#[test]
fn plaintext_local_provider_over_http1() {
    let driver = driver();
    let (world, spec) = direct_world(
        "direct-transport-local-http1",
        "transport",
        r#"["observe__workspace"]"#,
        RETRY_FAST,
    );
    world.write_project_file("a.txt", "alpha\n").unwrap();
    world.write_project_file("b.txt", "beta\n").unwrap();
    let server = FakeLlmServer::start_plain_http1(sequence(vec![
        tool_call(
            "call-1",
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"a.txt"}}"#,
        ),
        SseScript::HttpError {
            status: 503,
            body: r#"{"error":"server busy, please try again"}"#.into(),
        },
        tool_call(
            "call-2",
            "observe__workspace",
            r#"{"request":{"mode":"read_file","path":"b.txt"}}"#,
        ),
        SseScript::plain("local answer"),
    ]))
    .unwrap();
    let base_url = server.base_url();
    world.set_env("OPENAI_COMPATIBLE_BASE_URL", &base_url);
    world.set_env("OPENAI_COMPATIBLE_MODEL", "transport");
    world.set_env("OPENAI_COMPATIBLE_API_KEY", FAKE_API_KEY);
    let evidence = evidence(&world, &spec);
    let send = SessionSendDriver::new(&driver, &world, &spec, "transport");
    let outcome = send.send("read both files locally").unwrap();
    let captures = server.captured_requests();
    let result = (|| {
        require(
            base_url.starts_with("http://localhost:"),
            format!("plaintext loopback base URL: {base_url}"),
        )?;
        assert_clean_send(&outcome)?;
        require(outcome.has_completed(), "local turn completes")?;
        require(
            captures.len() == 4,
            format!("4 model requests, got {}", captures.len()),
        )?;
        assert_identity(&captures, send.session_id().unwrap())?;
        let connections: Vec<_> = captures.iter().map(|c| c.connection).collect();
        require(
            connections == [0, 0, 0, 0],
            format!("every request of the turn reuses one connection: {connections:?}"),
        )?;
        let turn = final_turn(&outcome);
        let second = attempts(&turn, 1);
        require(
            second.len() == 2
                && second[0]["end"]["cause"]
                    == serde_json::json!({"cause":"provider_error","retryable":true}),
            format!("the 503 is one retryable attempt before success: {second:?}"),
        )?;
        let line = diagnostic(&second[0]);
        require(
            line == "HTTP 503: server busy, please try again",
            format!("503 diagnostic: {line:?}"),
        )
    })();
    finish_scenario(
        evidence,
        &[("local http1 turn", &outcome.command)],
        captures,
        result,
    );
}
